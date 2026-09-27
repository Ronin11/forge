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

/// `FORGE_HOME/bin/<CAPABILITY>`: the pid of the daemon worker that claims,
/// written once it has told systemd so. A live pid there is a worker that
/// starts a successor on a staged release, so `deploy-self` only stages;
/// the old worker waits for its successor's pid here before it exits.
pub const CAPABILITY: &str = "successor-capable";

/// The unit a worker runs under when systemd does not say otherwise.
pub const WORKER_UNIT: &str = "forge-worker";

/// How long a drained worker waits for its successor to take the unit over.
const HANDOVER_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

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
            write_capability(&root, pid);
            match f.store.apply_contracts(&version, crate::worker::pid_alive) {
                Ok(0) => {}
                Ok(n) => eprintln!("applied {n} contract migration step(s)"),
                Err(e) => eprintln!("contract migration failed: {e:#}"),
            }
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

    /// A successor under systemd whose unit has a stop job: an operator's
    /// own `systemctl --user stop` or `restart` queued while the old worker
    /// was the main pid, whose SIGTERM systemd never re-sends to the pid
    /// that took the unit over. The worker drains and exits as on SIGTERM.
    pub fn stop_requested(&self) -> bool {
        if !self.daemon
            || std::env::var_os(SUCCESSOR_OF).is_none()
            || std::env::var_os("NOTIFY_SOCKET").is_none()
        {
            return false;
        }
        unit_state(&own_unit()).as_deref() == Some("deactivating")
    }

    /// Deregister; a worker that started a successor first waits (bounded)
    /// until the successor has taken the unit over, so systemd never sees
    /// the main pid exit before `MAINPID=` moved it.
    pub fn leave(&mut self, f: &Forge) {
        if !self.daemon {
            return;
        }
        if let Some((child, _)) = &mut self.child {
            let root = release::root(&f.paths.home);
            let want = i64::from(child.id());
            let start = std::time::Instant::now();
            while read_capability(&root) != Some(want)
                && matches!(child.try_wait(), Ok(None))
                && start.elapsed() < HANDOVER_WAIT
            {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
        let _ = f.store.stop_worker(self.id);
    }
}

/// Whether the worker that would run a deploy of Forge starts successors:
/// a live worker registered in the workers table (only a release that
/// starts successors registers there), or a live pid in the capability
/// file.
pub fn capable(home: &std::path::Path, store: &crate::store::Store) -> bool {
    store
        .live_workers(pid_alive)
        .is_ok_and(|live| !live.is_empty())
        || read_capability(&release::root(home)).is_some_and(pid_alive)
}

fn read_capability(root: &std::path::Path) -> Option<i64> {
    std::fs::read_to_string(root.join(CAPABILITY))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn write_capability(root: &std::path::Path, pid: i64) {
    let tmp = root.join(format!(".{CAPABILITY}.{pid}"));
    let written = std::fs::create_dir_all(root)
        .and_then(|()| std::fs::write(&tmp, format!("{pid}\n")))
        .and_then(|()| std::fs::rename(&tmp, root.join(CAPABILITY)));
    if let Err(e) = written {
        eprintln!("could not write {}: {e}", root.join(CAPABILITY).display());
    }
}

/// The systemd unit this process runs in, from its cgroup; the worker unit
/// when that says nothing.
fn own_unit() -> String {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|c| {
            c.lines()
                .flat_map(|l| l.rsplit('/'))
                .find(|seg| seg.ends_with(".service"))
                .map(|seg| seg.trim_end_matches(".service").to_string())
        })
        .unwrap_or_else(|| WORKER_UNIT.to_string())
}

/// `systemctl --user is-active <unit>`'s word for it: active,
/// deactivating, inactive...; None when systemctl cannot be run.
pub fn unit_state(unit: &str) -> Option<String> {
    let out = Command::new("systemctl")
        .args(["--user", "is-active", unit])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let word = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!word.is_empty()).then_some(word)
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
