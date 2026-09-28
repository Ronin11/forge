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
use crate::plugins::Supervisor;
use crate::release;
use crate::worker::{WorkOpts, pid_alive};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

/// The units restarted when the self deploy target does not declare any:
/// its `units` arg is what `deploy-self` reads, and this is the one Forge
/// cannot run without.
const DEFAULT_UNITS: &str = "forge-web";

/// How many half-second polls a restarted unit gets to report active when
/// the target declares no `tries`, as in `deploy-self`.
const DEFAULT_TRIES: u32 = 40;
const ACTIVE_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// Set on a successor: the pid of the worker that started it.
const SUCCESSOR_OF: &str = "FORGE_SUCCESSOR_OF";

/// `FORGE_HOME/bin/<CAPABILITY>`: the pid of the daemon worker that claims,
/// written once it has told systemd so. A live pid there is a worker that
/// starts a successor on a staged release, so `deploy-self` only stages;
/// the old worker waits for its successor's pid here before it exits.
pub const CAPABILITY: &str = "successor-capable";

/// `FORGE_HOME/bin/<STARTED>`: `<pid> <release>` of the successor this home's
/// worker last started. The old worker exits 0 once the successor names
/// itself in the capability file, and cannot watch it after that, so the
/// record outlives it: a worker restarted on `current` finds the successor
/// dead with `current` never flipped and marks the release failed.
const STARTED: &str = "successor-started";

/// `FORGE_HOME/bin/<FAILED>`: `<release> <unix seconds>` of a staged release
/// whose successor died. `staged` naming it is not started again until a
/// deploy stages a release anew.
pub const FAILED: &str = "staged-failed";

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
                take_over(f, &root, &version);
            } else {
                settle_started(&root, &version);
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
    /// Starts the successor when a newer release is staged. The plugins stay
    /// here until the successor has claimed, and come back here when it
    /// exits without (or after) claiming while this worker still does.
    pub async fn superseded(
        &mut self,
        f: &Arc<Forge>,
        plugins: &mut Option<Supervisor>,
    ) -> Result<bool> {
        if !self.daemon {
            return Ok(false);
        }
        if let Some((child, id)) = &mut self.child
            && let Ok(Some(status)) = child.try_wait()
        {
            eprintln!("successor {} exited ({status})", child.id());
            let root = release::root(&f.paths.home);
            if release::pointed_at(&root, "current").as_deref() != Some(id.as_str()) {
                mark_failed(&root, id);
            }
            self.failed.insert(std::mem::take(id));
            self.child = None;
        }
        let live = f.store.live_workers(pid_alive)?;
        let newer: Vec<_> = live
            .iter()
            .filter(|w| w.id > self.id && w.version != self.version)
            .collect();
        let claimant = read_capability(&release::root(&f.paths.home))
            .filter(|pid| newer.iter().any(|w| w.pid == *pid));
        crate::plugins::handoff::settle(f, plugins, claimant).await;
        if !newer.is_empty() {
            return Ok(true);
        }
        if let Some(next) = self.staged_successor(&f.paths, &live) {
            match self.spawn(&f.paths, &next) {
                Ok(child) => {
                    write_started(&release::root(&f.paths.home), i64::from(child.id()), &next);
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
                    mark_failed(&release::root(&f.paths.home), &next);
                    self.failed.insert(next);
                }
            }
        }
        Ok(false)
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
            && failed_release(&root).as_deref() != Some(staged.as_str())
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
    pub fn leave(&mut self, f: &Forge) -> Result<()> {
        if !self.daemon {
            return Ok(());
        }
        let mut result = Ok(());
        if let Some((child, id)) = &mut self.child {
            let root = release::root(&f.paths.home);
            let want = i64::from(child.id());
            let start = std::time::Instant::now();
            while read_capability(&root) != Some(want)
                && matches!(child.try_wait(), Ok(None))
                && start.elapsed() < HANDOVER_WAIT
            {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            if read_capability(&root) != Some(want) {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    mark_failed(&root, id);
                }
                result = Err(match child.try_wait() {
                    Ok(Some(status)) => anyhow::anyhow!(
                        "successor pid {want} for release {id} exited ({status}) without taking the unit over"
                    ),
                    _ => anyhow::anyhow!(
                        "successor pid {want} for release {id} did not take the unit over within {} s",
                        HANDOVER_WAIT.as_secs()
                    ),
                });
            }
        }
        let _ = f.store.stop_worker(self.id);
        result
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

/// The release recorded in `staged-failed`, if any.
fn failed_release(root: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(FAILED)).ok()?;
    text.split_whitespace().next().map(str::to_string)
}

/// Write `name` under `root` through a temporary file renamed into place.
fn write_record(root: &std::path::Path, name: &str, text: &str) {
    let tmp = root.join(format!(".{name}.{}", std::process::id()));
    let written = std::fs::create_dir_all(root)
        .and_then(|()| std::fs::write(&tmp, text))
        .and_then(|()| std::fs::rename(&tmp, root.join(name)));
    if let Err(e) = written {
        eprintln!("could not write {}: {e}", root.join(name).display());
    }
}

fn write_started(root: &std::path::Path, pid: i64, release: &str) {
    write_record(root, STARTED, &format!("{pid} {release}\n"));
}

fn read_started(root: &std::path::Path) -> Option<(i64, String)> {
    let text = std::fs::read_to_string(root.join(STARTED)).ok()?;
    let mut words = text.split_whitespace();
    Some((words.next()?.parse().ok()?, words.next()?.to_string()))
}

/// Record `release` as one whose successor died, and retire `staged` when
/// it still names it, so no worker starts it again.
fn mark_failed(root: &std::path::Path, release: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    write_record(root, FAILED, &format!("{release} {now}\n"));
    if release::pointed_at(root, "staged").as_deref() == Some(release) {
        let _ = std::fs::remove_file(root.join("staged"));
    }
    eprintln!("release {release} failed as a successor: not started again (bin/{FAILED})");
}

/// What became of the successor the last worker started: gone, with
/// `current` never moved to its release, it failed after taking over (or
/// before) and this restarted worker records it; one that flipped `current`
/// or is this release needs no record any more.
fn settle_started(root: &std::path::Path, version: &str) {
    let Some((pid, release)) = read_started(root) else {
        return;
    };
    if pid_alive(pid) && release != version {
        return;
    }
    if release != version && release::pointed_at(root, "current").as_deref() != Some(&release) {
        mark_failed(root, &release);
    }
    let _ = std::fs::remove_file(root.join(STARTED));
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

/// What became of one unit the successor restarted.
#[derive(Debug, PartialEq)]
enum Unit {
    /// Restarted and, once waited on, active.
    Restarted,
    /// systemd has no such unit on this machine: a note, not a failure.
    NotFound,
    /// systemctl cannot be run at all (no systemd here): a note too.
    NoSystemctl(String),
    /// Refused the restart, or did not come back active in time.
    Failed(String),
}

impl Unit {
    /// The unit's own line in the successor's log.
    fn describe(&self, unit: &str) -> String {
        match self {
            Unit::Restarted => format!("restarted {unit}"),
            Unit::NotFound => format!("{unit}: unit not found"),
            Unit::NoSystemctl(why) => format!("{unit}: not restarted, {why}"),
            Unit::Failed(why) => format!("{unit}: {why}"),
        }
    }

    /// Only a unit that did not come back is a failure.
    fn failed(&self) -> bool {
        matches!(self, Unit::Failed(_))
    }
}

/// The units to restart and how long each gets to come back: the self
/// deploy target's `units` and `tries` args, read the way `deploy-self`
/// reads them (blanks or commas between names).
fn declared_units(target: Option<&crate::store::DeployTarget>) -> (Vec<String>, u32) {
    let arg = |k: &str| target.and_then(|t| t.args.get(k));
    let list = arg("units").map_or(DEFAULT_UNITS, String::as_str);
    let mut units: Vec<String> = list
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .collect();
    if units.is_empty() {
        units.push(DEFAULT_UNITS.to_string());
    }
    let tries = arg("tries")
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(DEFAULT_TRIES);
    (units, tries)
}

/// Ask systemd to restart `unit` (without waiting for the job).
fn restart_unit(unit: &str) -> Unit {
    let out = match Command::new("systemctl")
        .args(["--user", "restart", "--no-block", unit])
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) => out,
        Err(e) => return Unit::NoSystemctl(format!("cannot run systemctl: {e}")),
    };
    if out.status.success() {
        return Unit::Restarted;
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let why = stderr.trim();
    // systemctl exits 5 for a unit that is not installed.
    if out.status.code() == Some(5) || why.contains("not found") {
        return Unit::NotFound;
    }
    Unit::Failed(if why.is_empty() {
        format!("restart refused ({})", out.status)
    } else {
        format!("restart refused: {why}")
    })
}

/// Poll `active` up to `tries` times, `pause` apart, until it says the
/// restarted unit is up; `Failed` when it never does.
fn await_active(mut active: impl FnMut() -> bool, tries: u32, pause: std::time::Duration) -> Unit {
    for n in 0..tries.max(1) {
        if active() {
            return Unit::Restarted;
        }
        if n + 1 < tries {
            std::thread::sleep(pause);
        }
    }
    Unit::Failed(format!("did not become active within {tries} tries"))
}

/// Restart `units` one by one and wait, each on its own, for the restarted
/// ones to report active; `(unit, what became of it)` in the order given.
fn restart_all(units: &[String], tries: u32) -> Vec<(String, Unit)> {
    units
        .iter()
        .map(|u| {
            let restarted = restart_unit(u);
            (u.clone(), restarted)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|(u, restarted)| {
            let outcome = match restarted {
                Unit::Restarted => await_active(
                    || unit_state(&u).as_deref() == Some("active"),
                    tries,
                    ACTIVE_POLL,
                ),
                other => other,
            };
            (u, outcome)
        })
        .collect()
}

/// The successor is live: `current` moves to its release and the units
/// the self deploy target declares restart on it, each reported on its own.
/// A unit that does not exist here is a note; one that does not come back
/// active in the bounded wait puts `current` back and restarts the units
/// on the release it named, as `deploy-self` does. Nothing to do when a
/// deploy already flipped `current`.
fn take_over(f: &Forge, root: &std::path::Path, version: &str) {
    let lock = match release::lock(root) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("could not take the release lock to flip current to {version}: {e:#}");
            return;
        }
    };
    if release::pointed_at(root, "current").as_deref() == Some(version) {
        return;
    }
    let was = match release::flip(&lock, root, version) {
        Ok(was) => was,
        Err(e) => {
            eprintln!("could not flip current to {version}: {e:#}");
            return;
        }
    };
    let target = f
        .store
        .deploy_target_by_method(crate::deploy::SELF_METHOD)
        .unwrap_or_else(|e| {
            eprintln!("could not read the self deploy target: {e:#}");
            None
        });
    let (units, tries) = declared_units(target.as_ref());
    let results = restart_all(&units, tries);
    for (unit, outcome) in &results {
        eprintln!("{}", outcome.describe(unit));
    }
    if !results.iter().any(|(_, o)| o.failed()) {
        return;
    }
    match release::restore(&lock, root, &was) {
        Ok(()) => {
            eprintln!(
                "putting current back to {}",
                was.0.as_deref().unwrap_or("nothing")
            );
            if was.0.is_some() {
                for unit in &units {
                    let _ = restart_unit(unit);
                }
            }
        }
        Err(e) => eprintln!("could not put current back: {e:#}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn target(args: &[(&str, &str)]) -> crate::store::DeployTarget {
        crate::store::DeployTarget {
            project: "forge".into(),
            name: "self".into(),
            repo: "/r".into(),
            scope: None,
            method: "deploy-self".into(),
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            check_cmd: String::new(),
            on_landing: true,
            smoke_url: None,
        }
    }

    #[test]
    fn each_unit_is_reported_on_its_own_and_only_a_unit_that_did_not_come_back_fails() {
        let lines = [
            (Unit::Restarted.describe("forge-web"), "restarted forge-web"),
            (
                Unit::NotFound.describe("forge-portal"),
                "forge-portal: unit not found",
            ),
            (
                Unit::Failed("did not become active within 3 tries".into()).describe("forge-web"),
                "forge-web: did not become active within 3 tries",
            ),
        ];
        for (got, want) in lines {
            assert_eq!(got, want);
        }
        assert!(!Unit::Restarted.failed());
        assert!(!Unit::NotFound.failed());
        assert!(!Unit::NoSystemctl("x".into()).failed());
        assert!(Unit::Failed("x".into()).failed());
    }

    #[test]
    fn the_units_come_from_the_self_target_and_default_to_forge_web() {
        assert_eq!(declared_units(None), (vec!["forge-web".to_string()], 40));
        assert_eq!(
            declared_units(Some(&target(&[]))),
            (vec!["forge-web".to_string()], 40)
        );
        let t = target(&[
            ("units", "forge-web, forge-portal  forge-extra"),
            ("tries", "3"),
        ]);
        assert_eq!(
            declared_units(Some(&t)),
            (
                vec![
                    "forge-web".to_string(),
                    "forge-portal".to_string(),
                    "forge-extra".to_string()
                ],
                3
            )
        );
        assert_eq!(
            declared_units(Some(&target(&[("units", " , ")]))).0,
            vec!["forge-web".to_string()]
        );
    }

    #[test]
    fn a_unit_gets_a_bounded_wait_to_report_active() {
        let mut polls = 0;
        let up = await_active(
            || {
                polls += 1;
                polls == 3
            },
            5,
            Duration::ZERO,
        );
        assert_eq!(up, Unit::Restarted);
        assert_eq!(polls, 3);

        let mut polls = 0;
        let down = await_active(
            || {
                polls += 1;
                false
            },
            4,
            Duration::ZERO,
        );
        assert_eq!(polls, 4);
        assert_eq!(
            down.describe("forge-web"),
            "forge-web: did not become active within 4 tries"
        );
    }
}
