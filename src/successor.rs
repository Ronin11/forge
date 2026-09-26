//! The successor worker (docs/OPS.md, "The running binary"). When
//! `FORGE_HOME/bin/staged` names a release this worker is not running, it
//! starts `forge work` from that release and registers it in the workers
//! table; from then on it claims nothing, finishes what it holds and exits,
//! while the successor claims. Claims go to the newest live version, so a
//! fix is live without waiting for a drain.
//!
//! Under systemd the unit is `Type=notify` with `NotifyAccess=all`: the
//! successor tells the manager `MAINPID=<its pid>`, so `systemctl --user
//! status forge-worker` names the worker that claims, with the draining
//! one listed beside it in the unit's cgroup.

use crate::ctx::{Forge, Paths};
use crate::release;
use crate::worker::{WorkOpts, pid_alive};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

/// The units that run this release's other binaries, restarted once the
/// successor has flipped `current` under them.
const UNITS: &[&str] = &["forge-web", "forge-portal"];

/// Set on a successor: the pid of the worker that started it.
const SUCCESSOR_OF: &str = "FORGE_SUCCESSOR_OF";

pub struct Succession {
    /// Only a daemon worker takes part; `forge work --once` neither yields
    /// nor supersedes.
    daemon: bool,
    version: String,
    id: i64,
    jobs: usize,
    poll: Option<u64>,
    max_tasks: Option<u32>,
    child: Option<(Child, String)>,
    /// Staged releases whose successor died: not started again.
    failed: HashSet<String>,
}

impl Succession {
    /// Register this worker under its release; a successor also flips
    /// `current` to it and restarts the units that run beside the worker.
    pub fn join(f: &Forge, opts: &WorkOpts) -> Result<Succession> {
        let root = release::root(&f.paths.home);
        let version = release::running(&root);
        let daemon = opts.poll.is_some();
        let pid = std::process::id() as i64;
        let id = if daemon {
            f.store.register_worker(pid, &version)?
        } else {
            0
        };
        if daemon {
            if let Ok(parent) = std::env::var(SUCCESSOR_OF) {
                eprintln!("successor of worker {parent}: release {version} claims from here");
                take_over(&root, &version);
            }
            notify(&format!("MAINPID={pid}\nREADY=1"));
        }
        Ok(Succession {
            daemon,
            version,
            id,
            jobs: opts.jobs,
            poll: opts.poll,
            max_tasks: opts.max_tasks,
            child: None,
            failed: HashSet::new(),
        })
    }

    /// Whether a newer version is live, so this worker claims no more.
    /// Starts the successor first when a newer release is staged.
    pub fn superseded(&mut self, f: &Forge) -> Result<bool> {
        if !self.daemon {
            return Ok(false);
        }
        if let Some((child, id)) = &mut self.child
            && let Ok(Some(status)) = child.try_wait()
        {
            eprintln!("successor {} exited ({status})", child.id());
            self.failed.insert(std::mem::take(id));
            self.child = None;
        }
        let live = f.store.live_workers(pid_alive)?;
        if !live
            .iter()
            .any(|w| w.id > self.id && w.version != self.version)
        {
            if let Some(next) = self.staged_successor(&f.paths, &live) {
                match self.spawn(&f.paths, &next) {
                    Ok(child) => {
                        f.store.register_worker(i64::from(child.id()), &next)?;
                        eprintln!(
                            "release {next} staged: successor pid {} started; this worker drains",
                            child.id()
                        );
                        self.child = Some((child, next));
                        return Ok(true);
                    }
                    Err(e) => {
                        eprintln!("release {next} staged but its worker did not start: {e:#}");
                        self.failed.insert(next);
                    }
                }
            }
            return Ok(false);
        }
        Ok(true)
    }

    /// The staged release to start a worker on: not this one, not one that
    /// already has a live worker or already failed to start.
    fn staged_successor(&self, paths: &Paths, live: &[crate::store::WorkerRow]) -> Option<String> {
        let root = release::root(&paths.home);
        let staged = release::pointed_at(&root, "staged")?;
        let runnable = release::release_dir(&root, &staged).join("forge").is_file();
        (staged != self.version
            && runnable
            && !self.failed.contains(&staged)
            && !live.iter().any(|w| w.version == staged))
        .then_some(staged)
    }

    /// `forge work` from release `id`, in its own process group (a signal
    /// to this worker's is not the successor's), with this unit's
    /// environment and the same arguments.
    fn spawn(&self, paths: &Paths, id: &str) -> Result<Child> {
        let bin = release::release_dir(&release::root(&paths.home), id).join("forge");
        let mut cmd = Command::new(&bin);
        cmd.arg("work").args(["--jobs", &self.jobs.to_string()]);
        if let Some(poll) = self.poll {
            cmd.args(["--poll", &poll.to_string()]);
        }
        if let Some(n) = self.max_tasks {
            cmd.args(["--max-tasks", &n.to_string()]);
        }
        cmd.env("FORGE_HOME", &paths.home)
            .env("FORGE_RELEASE", id)
            .env(SUCCESSOR_OF, std::process::id().to_string())
            .env_remove("FORGE_BIN")
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting {}", bin.display()))
    }

    pub fn leave(&self, f: &Forge) {
        if self.daemon {
            let _ = f.store.stop_worker(self.id);
        }
    }
}

/// The successor is live: `current` moves to its release and the units
/// that run this release's other binaries restart on it. Nothing to do
/// when a deploy already flipped `current`.
fn take_over(root: &std::path::Path, version: &str) {
    if release::pointed_at(root, "current").as_deref() == Some(version) {
        return;
    }
    if let Err(e) = release::flip(root, version) {
        eprintln!("could not flip current to {version}: {e:#}");
        return;
    }
    let restarted = Command::new("systemctl")
        .args(["--user", "restart", "--no-block"])
        .args(UNITS)
        .status();
    if !restarted.is_ok_and(|s| s.success()) {
        eprintln!("could not restart {}", UNITS.join(", "));
    }
}

/// Tell systemd (`Type=notify`) something about this service; nothing when
/// not run by it.
fn notify(state: &str) {
    use std::os::unix::ffi::OsStrExt;
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    let Ok(sock) = std::os::unix::net::UnixDatagram::unbound() else {
        return;
    };
    let bytes = path.as_bytes();
    let sent = match bytes.strip_prefix(b"@") {
        #[cfg(target_os = "linux")]
        Some(name) => {
            use std::os::linux::net::SocketAddrExt;
            std::os::unix::net::SocketAddr::from_abstract_name(name)
                .and_then(|addr| sock.send_to_addr(state.as_bytes(), &addr))
        }
        _ => sock.send_to(state.as_bytes(), std::path::Path::new(&path)),
    };
    if let Err(e) = sent {
        eprintln!("sd_notify: {e}");
    }
}
