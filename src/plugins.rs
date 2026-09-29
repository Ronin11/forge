//! Plugins: a directory whose base name matches its manifest name, holding
//! `plugin.toml`. Discovery follows the same shape as `src/workflows.rs`
//! and for the same reason: one broken `plugin.toml` must not stop the
//! others loading. See docs/PLUGINS.md.

pub mod drift;
pub mod handoff;

use crate::ctx::Forge;
use crate::workflows::Problem;
use anyhow::{Context, Result, bail};
use handoff::{Reason, StopReason, reason_text};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Events,
    Intake,
    Annotate,
    Message,
    System,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Events => "events",
            Capability::Intake => "intake",
            Capability::Annotate => "annotate",
            Capability::Message => "message",
            Capability::System => "system",
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The supervision policy. `on-failure` (the default) restarts only on a
/// non-zero exit; `always` restarts unconditionally; `never` leaves it
/// stopped. See `Supervisor` below and docs/PLUGINS.md.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Restart {
    Always,
    #[default]
    OnFailure,
    Never,
}

impl Restart {
    pub fn as_str(self) -> &'static str {
        match self {
            Restart::Always => "always",
            Restart::OnFailure => "on-failure",
            Restart::Never => "never",
        }
    }
}

impl std::fmt::Display for Restart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRaw {
    name: String,
    #[serde(default)]
    description: String,
    run: Vec<String>,
    build: Option<Vec<String>>,
    capabilities: Vec<Capability>,
    #[serde(default)]
    restart: Restart,
}

/// One plugin's `plugin.toml`, parsed and validated.
#[derive(Clone, Debug)]
pub struct Manifest {
    pub name: String,
    pub description: String,
    pub run: Vec<String>,
    pub build: Option<Vec<String>>,
    pub capabilities: BTreeSet<Capability>,
    pub restart: Restart,
}

fn parse_manifest(path: &Path, text: &str, dir_name: &str) -> Result<Manifest> {
    let raw: ManifestRaw =
        toml::from_str(text).with_context(|| format!("parsing {}", path.display()))?;
    if raw.name != dir_name {
        bail!(
            "{}: name {:?} does not match the directory name {:?}",
            path.display(),
            raw.name,
            dir_name
        );
    }
    if raw.run.is_empty() {
        bail!("{}: `run` is empty", path.display());
    }
    if raw.build.as_ref().is_some_and(|b| b.is_empty()) {
        bail!("{}: `build` is empty", path.display());
    }
    let capabilities: BTreeSet<Capability> = raw.capabilities.into_iter().collect();
    if capabilities.is_empty() {
        bail!("{}: `capabilities` is empty", path.display());
    }
    Ok(Manifest {
        name: raw.name,
        description: raw.description,
        run: raw.run,
        build: raw.build,
        capabilities,
        restart: raw.restart,
    })
}

/// One plugin as discovered: its manifest, its directory, and the root it
/// was found under.
#[derive(Clone, Debug)]
pub struct Plugin {
    pub name: String,
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub root: PathBuf,
}

/// Every plugin found across every root, loaded once: one read of each
/// root directory, per-file error capture. A `plugin.toml` that fails to
/// parse contributes a problem instead of aborting the load, so one bad
/// plugin does not hide the rest.
pub struct Catalog {
    pub plugins: BTreeMap<String, Plugin>,
    pub problems: Vec<Problem>,
    /// Plugins whose directory is there but whose `plugin.toml` could not
    /// be read or parsed (one being rewritten, say): present but broken,
    /// not absent.
    pub broken: BTreeSet<String>,
    /// Roots that exist but could not be listed: every plugin under one
    /// is unknown, not absent.
    pub unreadable: Vec<String>,
}

/// Discover plugins over the ordered roots: `<home>/plugins` first, then
/// every entry of `plugin_dirs` in the order given. An earlier root wins a
/// duplicate name; the shadowed copy is a non-blocking problem. A
/// `plugin_dirs` entry that does not exist is a non-blocking problem;
/// `<home>/plugins` not existing is not (nothing has been installed yet).
pub fn load_catalog(home: &Path, plugin_dirs: &[PathBuf]) -> Catalog {
    let mut plugins: BTreeMap<String, Plugin> = BTreeMap::new();
    let mut problems = Vec::new();
    let mut broken = BTreeSet::new();
    let mut unreadable = Vec::new();

    let roots: Vec<(PathBuf, bool)> = std::iter::once((home.join("plugins"), false))
        .chain(plugin_dirs.iter().cloned().map(|p| (p, true)))
        .collect();

    for (root, configured) in roots {
        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if configured {
                    problems.push(Problem {
                        file: root.display().to_string(),
                        blocking: false,
                        what: "directory does not exist".into(),
                    });
                }
                continue;
            }
            Err(e) => {
                unreadable.push(format!("{}: {e}", root.display()));
                problems.push(Problem {
                    file: root.display().to_string(),
                    blocking: false,
                    what: format!("{e}"),
                });
                continue;
            }
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();

        for dir in dirs {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            let manifest_path = dir.join("plugin.toml");
            let text = match std::fs::read_to_string(&manifest_path) {
                Ok(t) => t,
                Err(e) => {
                    broken.insert(name.clone());
                    problems.push(Problem {
                        file: format!("{name}/plugin.toml"),
                        blocking: true,
                        what: format!("{e}"),
                    });
                    continue;
                }
            };
            match parse_manifest(&manifest_path, &text, &name) {
                Ok(manifest) => {
                    if plugins.contains_key(&name) {
                        problems.push(Problem {
                            file: dir.display().to_string(),
                            blocking: false,
                            what: format!(
                                "plugin {name:?} is shadowed by an earlier root and not loaded"
                            ),
                        });
                        continue;
                    }
                    plugins.insert(
                        name,
                        Plugin {
                            name: manifest.name.clone(),
                            manifest,
                            dir: dir.clone(),
                            root: root.clone(),
                        },
                    );
                }
                Err(e) => {
                    broken.insert(name.clone());
                    problems.push(Problem {
                        file: format!("{name}/plugin.toml"),
                        blocking: true,
                        what: format!("{e:#}"),
                    });
                }
            }
        }
    }

    broken.retain(|name| !plugins.contains_key(name));
    Catalog {
        plugins,
        problems,
        broken,
        unreadable,
    }
}

/// Copies `src` into `<home>/plugins/<name>`, `<name>` taken from `src`'s own
/// base name, refusing a name already installed there. The manifest is
/// parsed and validated (same rules as `load_catalog`) before anything is
/// copied. Runs the manifest's `build` argv in the installed directory
/// afterward, if it has one. See docs/PLUGINS.md "The verbs".
pub fn install(home: &Path, src: &Path) -> Result<Manifest> {
    let name = src
        .file_name()
        .with_context(|| format!("{}: no directory name", src.display()))?
        .to_string_lossy()
        .into_owned();
    let manifest_path = src.join("plugin.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest = parse_manifest(&manifest_path, &text, &name)?;

    let dest = home.join("plugins").join(&name);
    if dest.exists() {
        bail!(
            "a plugin named {name:?} is already installed at {}",
            dest.display()
        );
    }
    copy_dir(src, &dest)
        .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;

    drift::record_install(&dest, src)?;
    run_build(&manifest, &dest)?;
    Ok(manifest)
}

/// Runs the manifest's `build` argv, if it has one, in `dir`.
fn run_build(manifest: &Manifest, dir: &Path) -> Result<()> {
    if let Some(build) = &manifest.build {
        let status = std::process::Command::new(&build[0])
            .args(&build[1..])
            .current_dir(dir)
            .status()
            .with_context(|| format!("running build {build:?} in {}", dir.display()))?;
        if !status.success() {
            bail!("build {build:?} failed in {}", dir.display());
        }
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
            let perms = std::fs::metadata(&from)?.permissions();
            std::fs::set_permissions(&to, perms)?;
        }
    }
    Ok(())
}

/// Removes `<home>/plugins/<name>`, the installed copy `install` made.
/// Clearing its enabled flag is the caller's job
/// (the same store update `forge plugin disable` makes; see `cli::deploy::plugin_uninstall`),
/// because that needs the store, which this module does not hold.
/// `<home>/plugins-state/<name>` is left alone, deliberately: it is the
/// plugin's own memory, not part of what was installed.
pub fn remove_installed(home: &Path, name: &str) -> Result<()> {
    let dir = home.join("plugins").join(name);
    if !dir.is_dir() {
        bail!("no installed plugin named {name:?} at {}", dir.display());
    }
    // Keep the directory intact until reconciliation has stopped supervision.
    // RunState alone is insufficient: backoff and failed starts can still
    // restart, and a stopped state can precede the process group's exit.
    let deadline =
        Instant::now() + Duration::from_secs(RECONCILE_SECS) + STOP_GRACE + Duration::from_secs(10);
    let _lock = loop {
        if let Some(lock) = try_lock_plugin(home, name) {
            break lock;
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for plugin {name:?} to stop; installed directory retained");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))
}

// --- Supervision -----------------------------------------------------
//
// A daemon `forge work` starts every enabled plugin and stops them when it
// drains (see `Supervisor`); `forge work --once` and `forge run`, a single
// task, never construct one.
// See docs/PLUGINS.md "Supervision" and "Environment".

const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const UPTIME_RESET: Duration = Duration::from_secs(60);
const STOP_GRACE: Duration = Duration::from_secs(10);
/// How often the supervisor re-reads the enabled set while the worker is
/// up, so `forge plugin enable`/`disable` takes effect without a restart.
const RECONCILE_SECS: u64 = 10;
/// An `events`-subscribed plugin polls `events.jsonl` for new bytes rather
/// than being pushed to (`cli::stats::events`'s 250ms sleep); stopping
/// right after the run that produced the last event would signal (and,
/// with the group now killed as a whole, terminate) that poll before it
/// can land. This settle window lets one more poll cycle happen before
/// `Supervisor::stop` reaches the plugins still up.
const STOP_SETTLE: Duration = Duration::from_millis(400);

/// What a plugin is doing right now, as the supervisor last recorded it.
/// Persisted to `<FORGE_HOME>/plugins-run/<name>.json` so `forge plugin
/// status`, its `--json`, and `forge doctor` can read it from another
/// process; not the plugin's own state (see `FORGE_PLUGIN_STATE`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum RunState {
    Running { pid: i64, since: i64 },
    Restarting { count: u32 },
    Stopped { last_exit: Option<String> },
}

impl RunState {
    /// A human line for `forge plugin status` and `forge doctor`.
    pub fn describe(&self) -> String {
        match self {
            RunState::Running { pid, since } => {
                format!(
                    "running pid {pid}, up {}s",
                    (crate::unix_now() - since).max(0)
                )
            }
            RunState::Restarting { count } => format!("restarting (x{count})"),
            RunState::Stopped { last_exit: Some(e) } => format!("stopped: {e}"),
            RunState::Stopped { last_exit: None } => "stopped".to_string(),
        }
    }
}

fn run_state_path(home: &Path, name: &str) -> PathBuf {
    home.join("plugins-run").join(format!("{name}.json"))
}

/// The last state the supervisor recorded for `name`, or `Stopped { last_exit: None }`
/// when nothing has ever run it (no worker has supervised it yet).
pub fn read_run_state(home: &Path, name: &str) -> RunState {
    std::fs::read_to_string(run_state_path(home, name))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(RunState::Stopped { last_exit: None })
}

fn write_run_state(home: &Path, name: &str, state: &RunState) {
    let path = run_state_path(home, name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(text) = serde_json::to_string(state) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn lock_path(home: &Path, name: &str) -> PathBuf {
    home.join("plugins-run").join(format!("{name}.lock"))
}

/// The exclusive `flock` that makes supervision single per home, or `None`
/// when another supervisor (a worker still draining, a duplicate) holds it.
/// The lock belongs to the open file description, not to the worker:
/// `spawn_plugin` hands that description to the plugin's process group
/// (clearing `FD_CLOEXEC` in a `pre_exec`), so it is released only when the
/// returned file has dropped, which the reconciler defers until the
/// supervising task has ended, *and* every member of the group has exited. A
/// worker that dies without stopping its plugins therefore leaves the lock
/// held, and its successor does not start a second copy beside the orphan.
fn try_lock_plugin(home: &Path, name: &str) -> Option<std::fs::File> {
    let path = lock_path(home, name);
    std::fs::create_dir_all(path.parent()?).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

fn restart_request_path(home: &Path, name: &str) -> PathBuf {
    home.join("plugins-run").join(format!("{name}.restart"))
}

/// The restart generation the supervisor has last seen for `name`; `0`
/// when `forge plugin restart` has never been called for it. A counter
/// rather than a timestamp so two requests inside the same second are
/// never coalesced into one.
fn read_restart_gen(home: &Path, name: &str) -> u64 {
    std::fs::read_to_string(restart_request_path(home, name))
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0)
}

/// Bumps `name`'s restart generation so a running supervisor's next
/// reconcile tick (see `Supervisor::start`) replaces its process with a
/// fresh one, without touching the enabled flag. This is the only way a
/// live plugin ever re-reads its own `FORGE_PLUGIN_DIR/config`: the
/// reference plugins only read it at startup (see docs/PLUGINS.md).
pub fn request_restart(home: &Path, name: &str) -> Result<()> {
    let path = restart_request_path(home, name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let next = read_restart_gen(home, name) + 1;
    let tmp = crate::release::temporary_path(path.parent().unwrap(), &format!("{name}.restart"));
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    file.write_all(next.to_string().as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn describe_exit(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(c) => format!("exit {c}"),
        None => match status.signal() {
            Some(s) => format!("signal {s}"),
            None => "unknown exit".to_string(),
        },
    }
}

async fn wait_for_stop(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

/// Sleeps `dur` unless a stop is requested first; `true` when it was.
async fn wait_backoff_or_stop(stop: &mut watch::Receiver<bool>, dur: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(dur) => false,
        _ = wait_for_stop(stop) => true,
    }
}

/// Observe exit without reaping, sweep the group, then reap the leader.
/// Reaping first opens a PID-reuse window: a later kill(-pgid) could hit
/// an unrelated group. WNOWAIT keeps the leader's PID reserved until the
/// sweep, and disarming the guard prevents another signal after reaping.
/// This must be the only waiter for this child, including during shutdown.
async fn wait_and_sweep(
    child: &mut Child,
    group: &mut GroupGuard,
) -> std::io::Result<std::process::ExitStatus> {
    if group.0 <= 1 {
        return child.wait().await;
    }
    loop {
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                group.0 as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                // Ownership was lost; the PID is no longer safe to signal.
                group.0 = 0;
            }
            return Err(error);
        }
        if unsafe { info.si_pid() } != 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    group.sweep();
    child.wait().await
}

/// Give the whole group a SIGTERM grace period. Sweep even if the leader
/// exits inside the grace: descendants may have ignored SIGTERM.
async fn stop_child(child: &mut Child, group: &mut GroupGuard) {
    if group.0 > 1 {
        unsafe {
            libc::kill(-group.0, libc::SIGTERM);
        }
    }
    if tokio::time::timeout(STOP_GRACE, wait_and_sweep(child, group))
        .await
        .is_err()
    {
        group.sweep();
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

/// `run` in the plugin directory, stdout/stderr appended to its log, and
/// exactly the environment docs/PLUGINS.md promises: `FORGE_BIN`,
/// `FORGE_HOME` (and, for one release, `FORGE2_HOME` too — the name
/// docs/PLUGINS.md promised before the rename, kept alongside the new one
/// so a plugin written against the old name still works), `FORGE_PLUGIN_DIR`,
/// `FORGE_PLUGIN_NAME`, `FORGE_PLUGIN_STATE`, plus the agent pass-through list
/// (`agent::agent_env`).
fn spawn_plugin(
    plugin: &Plugin,
    home: &Path,
    state_dir: &Path,
    log_path: &Path,
    lock: &std::fs::File,
) -> Result<Child> {
    let stdout_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let stderr_file = stdout_file.try_clone()?;
    let bin = crate::binary::launch_path()?;
    let mut cmd = Command::new(&plugin.manifest.run[0]);
    cmd.args(&plugin.manifest.run[1..])
        .current_dir(&plugin.dir)
        .env_clear()
        .envs(crate::agent::agent_env(crate::sandbox::Phase::Agent))
        .env("FORGE_BIN", bin)
        .env("FORGE_HOME", home)
        .env("FORGE2_HOME", home)
        .env("FORGE_PLUGIN_NAME", &plugin.name)
        .env("FORGE_PLUGIN_DIR", &plugin.dir)
        .env("FORGE_PLUGIN_STATE", state_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .kill_on_drop(true)
        // Its own process group, so `stop_child` can signal the whole
        // pipeline of a shell plugin, not just this leader.
        .process_group(0);
    // A worker that is killed outright (SIGKILL, a crash, a test harness
    // giving up on it) never runs `stop_child`; the kernel delivers SIGTERM
    // to the plugin instead of leaving it to be reparented to init.
    let parent = std::process::id() as libc::pid_t;
    // The plugin's group inherits the locked file description across exec,
    // so the `flock` lives as long as any member of the group does.
    let lock_fd = lock.as_raw_fd();
    unsafe {
        cmd.pre_exec(move || {
            let flags = libc::fcntl(lock_fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(lock_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The worker may have died between fork and here, before the
            // signal was armed: nothing would ever deliver it.
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    cmd.spawn()
        .with_context(|| format!("spawning {:?}", plugin.manifest.run))
}

/// Kills a plugin's whole process group when dropped, however the
/// supervising task ends (a normal stop, a restart, a panic, the runtime
/// being torn down): `kill_on_drop` alone reaches only the group leader,
/// and a shell plugin's `forge events --follow | while ...` pipeline
/// outlives its shell.
struct GroupGuard(libc::pid_t);

impl GroupGuard {
    fn sweep(&mut self) {
        if self.0 > 1 {
            unsafe {
                // ESRCH is normal when no group members remain.
                libc::kill(-self.0, libc::SIGKILL);
            }
            self.0 = 0;
        }
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.sweep();
    }
}

/// Records that `name` is stopped, and why.
fn write_stopped(home: &Path, name: &str, why: String) {
    write_run_state(
        home,
        name,
        &RunState::Stopped {
            last_exit: Some(why),
        },
    );
}

/// One plugin, started, restarted per its manifest's policy, and stopped
/// when `stop` fires. Runs until told to stop; a plugin's own failure
/// never propagates out of this task.
async fn supervise_plugin(
    home: PathBuf,
    plugin: Plugin,
    mut stop: watch::Receiver<bool>,
    reason: Reason,
    lock: std::fs::File,
) {
    let name = plugin.name.clone();
    let state_dir = home.join("plugins-state").join(&name);
    let _ = std::fs::create_dir_all(&state_dir);
    let log_path = home
        .join("logs")
        .join("plugins")
        .join(format!("{name}.log"));
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    let mut backoff = BACKOFF_START;
    let mut restarts: u32 = 0;

    loop {
        if *stop.borrow() {
            return;
        }

        let mut child = match spawn_plugin(&plugin, &home, &state_dir, &log_path, &lock) {
            Ok(c) => c,
            Err(e) => {
                write_stopped(&home, &name, format!("failed to start: {e:#}"));
                if plugin.manifest.restart == Restart::Never
                    || wait_backoff_or_stop(&mut stop, backoff).await
                {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
        };

        let pid = child.id().unwrap_or(0) as i64;
        let mut group = GroupGuard(pid as libc::pid_t);
        let started = Instant::now();
        write_run_state(
            &home,
            &name,
            &RunState::Running {
                pid,
                since: crate::unix_now(),
            },
        );

        let waited = tokio::select! {
            r = wait_and_sweep(&mut child, &mut group) => Some(r),
            _ = wait_for_stop(&mut stop) => None,
        };

        let status = match waited {
            None => {
                stop_child(&mut child, &mut group).await;
                write_stopped(&home, &name, reason_text(&reason));
                return;
            }
            Some(Ok(s)) => s,
            Some(Err(e)) => {
                write_stopped(&home, &name, format!("wait failed: {e:#}"));
                return;
            }
        };

        let desc = describe_exit(status);
        let should_restart = match plugin.manifest.restart {
            Restart::Always => true,
            Restart::OnFailure => !status.success(),
            Restart::Never => false,
        };
        if !should_restart {
            write_stopped(&home, &name, desc);
            return;
        }

        restarts += 1;
        write_run_state(&home, &name, &RunState::Restarting { count: restarts });
        if started.elapsed() >= UPTIME_RESET {
            backoff = BACKOFF_START;
        }
        let wait = backoff;
        backoff = (backoff * 2).min(BACKOFF_MAX);
        if wait_backoff_or_stop(&mut stop, wait).await {
            write_stopped(&home, &name, desc);
            return;
        }
    }
}

/// The enabled plugins as the reconciler sees them on one tick: those it
/// can run, and those the catalog lists as broken right now (a
/// `plugin.toml` mid-rewrite), which are neither started nor stopped.
struct Enabled {
    plugins: BTreeMap<String, Plugin>,
    broken: BTreeSet<String>,
}

/// Every enabled plugin right now: the catalog, over the roots of the
/// config the worker has validated (`f.plugin_dirs`, see `crate::reload`),
/// and the store's enabled flags, re-read each time so `forge plugin
/// enable`/`disable` is seen. An error from either read is an error, never
/// "nothing is enabled": the caller keeps what runs.
fn enabled_plugins_now(f: &Forge) -> Result<Enabled> {
    let enabled = f
        .store
        .enabled_plugins()
        .context("reading the enabled plugins")?;
    let cat = load_catalog(&f.paths.home, &f.plugin_dirs);
    if !cat.unreadable.is_empty() {
        bail!("listing plugin roots: {}", cat.unreadable.join("; "));
    }
    Ok(Enabled {
        plugins: cat
            .plugins
            .into_iter()
            .filter(|(name, _)| enabled.contains(name))
            .collect(),
        broken: cat
            .broken
            .into_iter()
            .filter(|name| enabled.contains(name))
            .collect(),
    })
}

/// One plugin this supervisor runs: its stop channel, its supervising task,
/// the restart generation it started under, and the per-home lock that
/// keeps every other supervisor off the plugin until the task has ended.
struct Supervised {
    stop: watch::Sender<bool>,
    handle: JoinHandle<()>,
    reason: Reason,
    restart_gen: u64,
    _lock: std::fs::File,
}

impl Supervised {
    fn spawn(home: &Path, plugin: &Plugin, restart_gen: u64, lock: std::fs::File) -> Supervised {
        let (stop, rx) = watch::channel(false);
        let reason = Reason::default();
        let handle = tokio::spawn(supervise_plugin(
            home.to_path_buf(),
            plugin.clone(),
            rx,
            reason.clone(),
            lock.try_clone().expect("duplicating the plugin lock"),
        ));
        Supervised {
            stop,
            handle,
            reason,
            restart_gen,
            _lock: lock,
        }
    }

    fn signal_stop(&self) {
        let _ = self.stop.send(true);
    }

    /// Stop for `why`: what `forge plugin status` says of the stopped plugin.
    async fn stop_because(self, why: StopReason) {
        if let Ok(mut r) = self.reason.lock() {
            *r = why;
        }
        self.stop().await;
    }

    async fn stop(self) {
        self.signal_stop();
        self.handle.await.ok();
    }
}

/// One reconcile tick over a successful read: replace what `forge plugin
/// restart` asked for, start what is enabled and not running, and stop
/// what is running and no longer enabled. A plugin the catalog lists as
/// broken is left as it is, running or not, until its manifest parses
/// again or its directory is gone.
async fn reconcile(f: &Forge, running: &mut BTreeMap<String, Supervised>, enabled: &Enabled) {
    let to_restart: Vec<String> = running
        .iter()
        .filter(|(name, s)| {
            enabled.plugins.contains_key(*name)
                && read_restart_gen(&f.paths.home, name) != s.restart_gen
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in to_restart {
        if let Some(s) = running.remove(&name) {
            s.stop().await;
        }
    }

    for (name, plugin) in &enabled.plugins {
        if !running.contains_key(name)
            && let Some(lock) = try_lock_plugin(&f.paths.home, name)
        {
            let restart_gen = read_restart_gen(&f.paths.home, name);
            running.insert(
                name.clone(),
                Supervised::spawn(&f.paths.home, plugin, restart_gen, lock),
            );
        }
    }
    let gone: Vec<String> = running
        .keys()
        .filter(|n| !enabled.plugins.contains_key(*n) && !enabled.broken.contains(*n))
        .cloned()
        .collect();
    for name in gone {
        if let Some(s) = running.remove(&name) {
            s.stop_because(StopReason::Disabled).await;
        }
    }
}

/// Supervises every enabled plugin for the life of `forge work`: starts
/// them, restarts them per their manifest's `restart` policy, and stops
/// them (SIGTERM, then SIGKILL after ten seconds) when told to. While
/// running it polls the enabled set every `RECONCILE_SECS` seconds, so
/// `forge plugin enable`/`disable` take effect within a few seconds
/// without needing the worker restarted; the same tick also notices a
/// `forge plugin restart` request and swaps a still-enabled plugin's
/// process for a fresh one, since `enable`/`disable` alone never
/// replaces a process that stayed enabled the whole time (see
/// `request_restart`). Supervision is single per home: each plugin is held
/// under an `flock` on `plugins-run/<name>.lock` for as long as its process
/// group lives (the group inherits the locked file description), and a plugin whose lock another supervisor holds is skipped and
/// tried again on the next tick. `stop` signals every plugin before it
/// waits on any, so the lock passes to a successor rather than overlapping.
pub struct Supervisor {
    stop: watch::Sender<bool>,
    /// The config the worker runs on, replaced by `reload` when it
    /// accepts an edit; the reconciler never reads `config.toml` itself.
    forge: watch::Sender<Arc<Forge>>,
    reconciler: JoinHandle<()>,
    /// Why the plugins still up when the supervisor stops are stopped.
    drain: Arc<Mutex<StopReason>>,
}

impl Supervisor {
    pub fn start(f: Arc<Forge>) -> Supervisor {
        Supervisor::start_every(f, Duration::from_secs(RECONCILE_SECS))
    }

    fn start_every(f: Arc<Forge>, every: Duration) -> Supervisor {
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let (forge_tx, forge_rx) = watch::channel(f);
        let drain = Arc::new(Mutex::new(StopReason::Worker));
        let drained = drain.clone();
        let reconciler = tokio::spawn(async move {
            // `restart_gen` is the generation this instance was started
            // with; a mismatch against `read_restart_gen` on a later tick
            // means `forge plugin restart` ran while it was up.
            let mut running: BTreeMap<String, Supervised> = BTreeMap::new();
            // The last read error logged, so a lasting one is logged once.
            let mut failing: Option<String> = None;
            loop {
                let f = forge_rx.borrow().clone();
                let reader = f.clone();
                let read = tokio::task::spawn_blocking(move || enabled_plugins_now(&reader));
                let result = tokio::select! {
                    biased;
                    _ = stop_rx.changed() => break,
                    result = read => result.context("joining plugin catalog reader").and_then(|r| r),
                };
                let enabled = match result {
                    Ok(e) => {
                        failing = None;
                        Some(e)
                    }
                    Err(e) => {
                        let text = format!("{e:#}");
                        if failing.as_ref() != Some(&text) {
                            eprintln!(
                                "plugins: cannot tell what is enabled, keeping the {} running: {text}",
                                running.len()
                            );
                            failing = Some(text);
                        }
                        None
                    }
                };
                if let Some(enabled) = enabled {
                    reconcile(&f, &mut running, &enabled).await;
                }

                if *stop_rx.borrow() {
                    break;
                }
                tokio::select! {
                    _ = tokio::time::sleep(every) => {}
                    _ = stop_rx.changed() => {}
                }
            }
            // Every path out of the loop above lands here with `running`
            // possibly non-empty: a plugin spawned earlier in the same tick
            // a stop was noticed (e.g. the replacement half of a restart
            // that raced the drain) must never be left running past
            // `Supervisor::stop`, so drain it once, unconditionally, rather
            // than only on the tick that first observes the stop signal.
            if !running.is_empty() {
                tokio::time::sleep(STOP_SETTLE).await;
            }
            let why = drained.lock().map(|r| *r).unwrap_or_default();
            for s in running.values() {
                if let Ok(mut r) = s.reason.lock() {
                    *r = why;
                }
                s.signal_stop();
            }
            for s in running.into_values() {
                s.handle.await.ok();
            }
        });
        Supervisor {
            stop: stop_tx,
            forge: forge_tx,
            reconciler,
            drain,
        }
    }

    /// The worker accepted a config edit (`crate::reload`): scan the
    /// plugin roots it names from the next tick on.
    pub fn reload(&self, f: Arc<Forge>) {
        self.forge.send_replace(f);
    }

    pub async fn stop(self) {
        let _ = self.stop.send(true);
        self.reconciler.await.ok();
    }
}

#[cfg(test)]
mod tests;
