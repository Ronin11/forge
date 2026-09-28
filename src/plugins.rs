//! Plugins: a directory whose base name matches its manifest name, holding
//! `plugin.toml`. Discovery follows the same shape as `src/workflows.rs`
//! and for the same reason: one broken `plugin.toml` must not stop the
//! others loading. See docs/PLUGINS.md.

pub mod drift;

use crate::ctx::Forge;
use crate::workflows::Problem;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
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
}

/// Discover plugins over the ordered roots: `<home>/plugins` first, then
/// every entry of `plugin_dirs` in the order given. An earlier root wins a
/// duplicate name; the shadowed copy is a non-blocking problem. A
/// `plugin_dirs` entry that does not exist is a non-blocking problem;
/// `<home>/plugins` not existing is not (nothing has been installed yet).
pub fn load_catalog(home: &Path, plugin_dirs: &[PathBuf]) -> Catalog {
    let mut plugins: BTreeMap<String, Plugin> = BTreeMap::new();
    let mut problems = Vec::new();

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
                    problems.push(Problem {
                        file: format!("{name}/plugin.toml"),
                        blocking: true,
                        what: format!("{e:#}"),
                    });
                }
            }
        }
    }

    Catalog { plugins, problems }
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
/// Stopping the plugin and clearing its enabled flag is the caller's job
/// (the same store update `forge plugin disable` makes; see `cli::deploy::plugin_uninstall`),
/// because that needs the store, which this module does not hold.
/// `<home>/plugins-state/<name>` is left alone, deliberately: it is the
/// plugin's own memory, not part of what was installed.
pub fn remove_installed(home: &Path, name: &str) -> Result<()> {
    let dir = home.join("plugins").join(name);
    if !dir.is_dir() {
        bail!("no installed plugin named {name:?} at {}", dir.display());
    }
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
/// Released when the returned file drops, which the reconciler defers until
/// the plugin's supervising task, and so its child, is gone.
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
    let tmp = path.with_extension("restart.tmp");
    std::fs::write(&tmp, next.to_string())?;
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

/// SIGTERM to the child's process group, then SIGKILL to the group ten
/// seconds later if it has not exited. `spawn_plugin` makes the child its
/// own group leader (`process_group(0)`), so its pid is also its pgid;
/// signalling the group reaches a shell plugin's pipeline, not just the
/// leader that outlives its own SIGTERM.
async fn stop_child(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    if tokio::time::timeout(STOP_GRACE, child.wait())
        .await
        .is_err()
    {
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

/// `run` in the plugin directory, stdout/stderr appended to its log, and
/// exactly the environment docs/PLUGINS.md promises: `FORGE_BIN`,
/// `FORGE_HOME` (and, for one release, `FORGE2_HOME` too — the name
/// docs/PLUGINS.md promised before the rename, kept alongside the new one
/// so a plugin written against the old name still works), `FORGE_PLUGIN_DIR`,
/// `FORGE_PLUGIN_NAME`, `FORGE_PLUGIN_STATE`, plus the pass-through list every agent and check
/// gets (`agent::agent_env`).
fn spawn_plugin(plugin: &Plugin, home: &Path, state_dir: &Path, log_path: &Path) -> Result<Child> {
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
        .envs(crate::agent::agent_env())
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
    unsafe {
        cmd.pre_exec(move || {
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

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if self.0 > 1 {
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
}

/// One plugin, started, restarted per its manifest's policy, and stopped
/// when `stop` fires. Runs until told to stop; a plugin's own failure
/// never propagates out of this task.
async fn supervise_plugin(home: PathBuf, plugin: Plugin, mut stop: watch::Receiver<bool>) {
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

        let mut child = match spawn_plugin(&plugin, &home, &state_dir, &log_path) {
            Ok(c) => c,
            Err(e) => {
                write_run_state(
                    &home,
                    &name,
                    &RunState::Stopped {
                        last_exit: Some(format!("failed to start: {e:#}")),
                    },
                );
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
        let _group = GroupGuard(pid as libc::pid_t);
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
            r = child.wait() => Some(r),
            _ = wait_for_stop(&mut stop) => None,
        };

        let status = match waited {
            None => {
                stop_child(&mut child).await;
                write_run_state(
                    &home,
                    &name,
                    &RunState::Stopped {
                        last_exit: Some("stopped by worker".to_string()),
                    },
                );
                return;
            }
            Some(Ok(s)) => s,
            Some(Err(e)) => {
                write_run_state(
                    &home,
                    &name,
                    &RunState::Stopped {
                        last_exit: Some(format!("wait failed: {e:#}")),
                    },
                );
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
            write_run_state(
                &home,
                &name,
                &RunState::Stopped {
                    last_exit: Some(desc),
                },
            );
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
            write_run_state(
                &home,
                &name,
                &RunState::Stopped {
                    last_exit: Some(desc),
                },
            );
            return;
        }
    }
}

/// Every enabled plugin right now: the catalog and the store's enabled
/// flags, re-read each time so `forge plugin enable`/`disable` is seen.
fn enabled_plugins_now(f: &Forge) -> BTreeMap<String, Plugin> {
    let Ok(cfg) = crate::config::load_home(&f.paths.home) else {
        return BTreeMap::new();
    };
    let cat = load_catalog(&f.paths.home, &cfg.plugin_dirs);
    let Ok(enabled) = f.store.enabled_plugins() else {
        return BTreeMap::new();
    };
    cat.plugins
        .into_iter()
        .filter(|(name, _)| enabled.contains(name))
        .collect()
}

/// One plugin this supervisor runs: its stop channel, its supervising task,
/// the restart generation it started under, and the per-home lock that
/// keeps every other supervisor off the plugin until the task has ended.
struct Supervised {
    stop: watch::Sender<bool>,
    handle: JoinHandle<()>,
    restart_gen: u64,
    _lock: std::fs::File,
}

impl Supervised {
    fn spawn(home: &Path, plugin: &Plugin, restart_gen: u64, lock: std::fs::File) -> Supervised {
        let (stop, rx) = watch::channel(false);
        let handle = tokio::spawn(supervise_plugin(home.to_path_buf(), plugin.clone(), rx));
        Supervised {
            stop,
            handle,
            restart_gen,
            _lock: lock,
        }
    }

    fn signal_stop(&self) {
        let _ = self.stop.send(true);
    }

    async fn stop(self) {
        self.signal_stop();
        self.handle.await.ok();
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
/// under an `flock` on `plugins-run/<name>.lock` for as long as its child
/// lives, and a plugin whose lock another supervisor holds is skipped and
/// tried again on the next tick. `stop` signals every plugin before it
/// waits on any, so the lock passes to a successor rather than overlapping.
pub struct Supervisor {
    stop: watch::Sender<bool>,
    reconciler: JoinHandle<()>,
}

impl Supervisor {
    pub fn start(f: Arc<Forge>) -> Supervisor {
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let reconciler = tokio::spawn(async move {
            // `restart_gen` is the generation this instance was started
            // with; a mismatch against `read_restart_gen` on a later tick
            // means `forge plugin restart` ran while it was up.
            let mut running: BTreeMap<String, Supervised> = BTreeMap::new();
            loop {
                let enabled = enabled_plugins_now(&f);

                let to_restart: Vec<String> = running
                    .iter()
                    .filter(|(name, s)| {
                        enabled.contains_key(*name)
                            && read_restart_gen(&f.paths.home, name) != s.restart_gen
                    })
                    .map(|(name, _)| name.clone())
                    .collect();
                for name in to_restart {
                    if let Some(s) = running.remove(&name) {
                        s.stop().await;
                    }
                }

                for (name, plugin) in &enabled {
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
                    .filter(|n| !enabled.contains_key(*n))
                    .cloned()
                    .collect();
                for name in gone {
                    if let Some(s) = running.remove(&name) {
                        s.stop().await;
                    }
                }

                if *stop_rx.borrow() {
                    break;
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(RECONCILE_SECS)) => {}
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
            for s in running.values() {
                s.signal_stop();
            }
            for s in running.into_values() {
                s.handle.await.ok();
            }
        });
        Supervisor {
            stop: stop_tx,
            reconciler,
        }
    }

    pub async fn stop(self) {
        let _ = self.stop.send(true);
        self.reconciler.await.ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `<root>/<name>/plugin.toml`; `root` is a root directory
    /// itself (what `load_catalog` scans directly, e.g. a `plugin_dirs`
    /// entry), not `<FORGE_HOME>`.
    fn write_plugin(root: &Path, name: &str, text: &str) {
        let plugin_dir = root.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.toml"), text).unwrap();
    }

    /// Writes `<home>/plugins/<name>/plugin.toml`, the built-in root
    /// `load_catalog` always scans first.
    fn write_manifest(home: &Path, name: &str, text: &str) {
        write_plugin(&home.join("plugins"), name, text);
    }

    #[test]
    fn manifest_parses_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "notify",
            "name = \"notify\"\ndescription = \"posts a notification\"\nrun = [\"./notify.sh\"]\ncapabilities = [\"events\"]\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(cat.problems.is_empty(), "{:?}", cat.problems);
        let p = cat.plugins.get("notify").unwrap();
        assert_eq!(p.manifest.description, "posts a notification");
        assert_eq!(p.manifest.run, vec!["./notify.sh"]);
        assert_eq!(p.manifest.build, None);
        assert_eq!(
            p.manifest.capabilities,
            [Capability::Events].into_iter().collect()
        );
        assert_eq!(p.manifest.restart, Restart::OnFailure, "the default");
    }

    #[test]
    fn manifest_parses_every_field() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "inbox",
            "name = \"inbox\"\ndescription = \"files tasks\"\nrun = [\"./inbox\"]\nbuild = [\"cargo\", \"build\", \"--release\"]\ncapabilities = [\"events\", \"intake\"]\nrestart = \"always\"\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(cat.problems.is_empty(), "{:?}", cat.problems);
        let p = cat.plugins.get("inbox").unwrap();
        assert_eq!(
            p.manifest.build,
            Some(vec!["cargo".into(), "build".into(), "--release".into()])
        );
        assert_eq!(
            p.manifest.capabilities,
            [Capability::Events, Capability::Intake]
                .into_iter()
                .collect()
        );
        assert_eq!(p.manifest.restart, Restart::Always);
    }

    #[test]
    fn unknown_fields_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "bad",
            "name = \"bad\"\nrun = [\"x\"]\ncapabilities = [\"events\"]\ntypo = true\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(!cat.plugins.contains_key("bad"));
        assert!(
            cat.problems
                .iter()
                .any(|p| p.blocking && p.what.contains("unknown field")),
            "{:?}",
            cat.problems
        );
    }

    #[test]
    fn empty_run_and_empty_capabilities_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "norun",
            "name = \"norun\"\nrun = []\ncapabilities = [\"events\"]\n",
        );
        write_manifest(
            dir.path(),
            "nocap",
            "name = \"nocap\"\nrun = [\"x\"]\ncapabilities = []\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(!cat.plugins.contains_key("norun"));
        assert!(!cat.plugins.contains_key("nocap"));
        assert!(
            cat.problems
                .iter()
                .any(|p| p.what.contains("`run` is empty"))
        );
        assert!(
            cat.problems
                .iter()
                .any(|p| p.what.contains("`capabilities` is empty"))
        );
    }

    #[test]
    fn an_unknown_capability_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "weird",
            "name = \"weird\"\nrun = [\"x\"]\ncapabilities = [\"tools\"]\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(!cat.plugins.contains_key("weird"));
        assert!(cat.problems.iter().any(|p| p.blocking));
    }

    #[test]
    fn the_directory_name_must_match_the_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "on-disk",
            "name = \"other\"\nrun = [\"x\"]\ncapabilities = [\"events\"]\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(cat.plugins.is_empty());
        assert!(
            cat.problems
                .iter()
                .any(|p| p.what.contains("does not match the directory name")),
            "{:?}",
            cat.problems
        );
    }

    #[test]
    fn an_earlier_root_wins_a_duplicate_name_and_the_shadowed_copy_is_a_warning() {
        let home = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        write_manifest(
            home.path(),
            "notify",
            "name = \"notify\"\ndescription = \"home copy\"\nrun = [\"./a\"]\ncapabilities = [\"events\"]\n",
        );
        write_plugin(
            extra.path(),
            "notify",
            "name = \"notify\"\ndescription = \"extra copy\"\nrun = [\"./b\"]\ncapabilities = [\"events\"]\n",
        );
        let cat = load_catalog(home.path(), &[extra.path().to_path_buf()]);
        let p = cat.plugins.get("notify").unwrap();
        assert_eq!(p.manifest.description, "home copy", "the earlier root wins");
        assert!(
            cat.problems
                .iter()
                .any(|p| !p.blocking && p.what.contains("shadowed")),
            "{:?}",
            cat.problems
        );
    }

    #[test]
    fn a_missing_configured_root_is_a_non_blocking_problem() {
        let home = tempfile::tempdir().unwrap();
        let missing = home.path().join("does-not-exist");
        let cat = load_catalog(home.path(), std::slice::from_ref(&missing));
        assert!(cat.plugins.is_empty());
        assert_eq!(cat.problems.len(), 1);
        assert!(!cat.problems[0].blocking);
        assert!(cat.problems[0].what.contains("does not exist"));
        assert_eq!(cat.problems[0].file, missing.display().to_string());
    }

    #[test]
    fn a_missing_home_plugins_dir_is_not_a_problem() {
        let home = tempfile::tempdir().unwrap();
        let cat = load_catalog(home.path(), &[]);
        assert!(cat.plugins.is_empty());
        assert!(cat.problems.is_empty());
    }

    #[test]
    fn one_broken_plugin_does_not_stop_the_others_loading() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "good",
            "name = \"good\"\nrun = [\"x\"]\ncapabilities = [\"events\"]\n",
        );
        write_manifest(dir.path(), "bad", "not valid toml [[[");
        let cat = load_catalog(dir.path(), &[]);
        assert!(cat.plugins.contains_key("good"));
        assert!(!cat.plugins.contains_key("bad"));
        assert!(cat.problems.iter().any(|p| p.blocking));
    }

    /// docs/REVIEW-3.md §3.1 item 8: a shell plugin's pipeline (`sleep 1000
    /// | cat`) must not outlive `Supervisor::stop`. `spawn_plugin` puts the
    /// plugin in its own process group and `stop_child` signals the group,
    /// so stopping the leader (the shell) also reaches the children it
    /// forked for the pipeline, which never install a SIGTERM trap of
    /// their own.
    #[tokio::test]
    async fn stop_kills_a_shell_plugins_whole_pipeline_not_just_its_leader() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "pipeline",
            "name = \"pipeline\"\nrun = [\"sh\", \"-c\", \"sleep 1000 | cat\"]\ncapabilities = [\"events\"]\nrestart = \"never\"\n",
        );
        let store = crate::store::Store::open(&dir.path().join("forge.db")).unwrap();
        let paths = crate::ctx::Paths {
            home: dir.path().to_path_buf(),
            worktrees: dir.path().join("worktrees"),
            logs: dir.path().join("logs"),
        };
        let f = crate::ctx::Forge::open_with(paths, store).unwrap();
        f.store
            .set_plugin_enabled("pipeline", true, crate::unix_now())
            .unwrap();
        let f = Arc::new(f);
        let sup = Supervisor::start(f.clone());

        let deadline = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            if let RunState::Running { pid, .. } = read_run_state(&f.paths.home, "pipeline") {
                break pid;
            }
            assert!(Instant::now() < deadline, "the plugin never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        // `sleep` and `cat`, the shell's own direct children for the
        // pipeline: waited for by pid, not group, so the assertion below
        // is not itself relying on the fix under test (`process_group(0)`)
        // to find them.
        let deadline = Instant::now() + Duration::from_secs(10);
        let children = loop {
            let out = std::process::Command::new("pgrep")
                .args(["-P", &pid.to_string()])
                .output()
                .unwrap();
            let kids: Vec<i32> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect();
            if kids.len() >= 2 {
                break kids;
            }
            assert!(
                Instant::now() < deadline,
                "the shell never forked its pipeline"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        sup.stop().await;

        // `kill(pid, 0)` still finds a just-killed process while it is a
        // zombie awaiting reaping by whatever it was reparented to (pid 1,
        // usually), which a busy machine can be slow to get to; a short
        // poll instead of one immediate check keeps this test about the
        // group having been signalled, not about reaper scheduling.
        let deadline = Instant::now() + Duration::from_secs(5);
        for kid in children {
            loop {
                let alive = unsafe { libc::kill(kid, 0) } == 0;
                if !alive {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "expected pipeline child {kid} reaped after stop"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    fn forge_enabling(dir: &Path, plugins: &[(&str, &str)]) -> Arc<crate::ctx::Forge> {
        let store = crate::store::Store::open(&dir.join("forge.db")).unwrap();
        let paths = crate::ctx::Paths {
            home: dir.to_path_buf(),
            worktrees: dir.join("worktrees"),
            logs: dir.join("logs"),
        };
        let f = crate::ctx::Forge::open_with(paths, store).unwrap();
        for (name, run) in plugins {
            let run = run.replace('\\', "\\\\").replace('"', "\\\"");
            write_manifest(
                dir,
                name,
                &format!(
                    "name = \"{name}\"\nrun = [\"sh\", \"-c\", \"{run}\"]\ncapabilities = [\"events\"]\nrestart = \"never\"\n"
                ),
            );
            f.store
                .set_plugin_enabled(name, true, crate::unix_now())
                .unwrap();
        }
        Arc::new(f)
    }

    async fn wait_running(home: &Path, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(read_run_state(home, name), RunState::Running { .. }) {
            assert!(Instant::now() < deadline, "{name} never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// docs/REVIEW-3.md §3.1 item 9: a draining worker's plugins are stopped
    /// together. Each plugin, on SIGTERM, waits for the other's SIGTERM to
    /// arrive before exiting, so a stop that waited on one plugin before
    /// signalling the next would leave both waiting out their own bound.
    #[tokio::test]
    async fn stop_signals_every_plugin_before_waiting_on_any_of_them() {
        let dir = tempfile::tempdir().unwrap();
        let script = |other: &str| {
            format!(
                "trap 'touch \"$FORGE_PLUGIN_STATE/term\"; i=0; while [ ! -e \"$FORGE_PLUGIN_STATE/../{other}/term\" ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done; [ -e \"$FORGE_PLUGIN_STATE/../{other}/term\" ] && touch \"$FORGE_PLUGIN_STATE/saw_other\"; exit 0' TERM; sleep 1000 & wait"
            )
        };
        let (a, b) = (script("b"), script("a"));
        let f = forge_enabling(dir.path(), &[("a", &a), ("b", &b)]);
        let sup = Supervisor::start(f.clone());
        wait_running(dir.path(), "a").await;
        wait_running(dir.path(), "b").await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        let started = Instant::now();
        sup.stop().await;
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "stop was sequential"
        );
        for name in ["a", "b"] {
            let seen = dir
                .path()
                .join("plugins-state")
                .join(name)
                .join("saw_other");
            assert!(seen.exists(), "{name} was signalled before the other was");
        }
    }

    /// docs/REVIEW-3.md §3.1 item 9: a supervisor that finds a plugin's
    /// `plugins-run/<name>.lock` held leaves the plugin alone; once the
    /// holder lets go, a supervisor starts it.
    #[tokio::test]
    async fn a_plugin_whose_lock_is_held_is_skipped_until_it_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let f = forge_enabling(dir.path(), &[("held", "sleep 1000")]);
        let lock = try_lock_plugin(dir.path(), "held").expect("first lock");
        assert!(try_lock_plugin(dir.path(), "held").is_none());

        let sup = Supervisor::start(f.clone());
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(matches!(
            read_run_state(dir.path(), "held"),
            RunState::Stopped { last_exit: None }
        ));
        sup.stop().await;

        drop(lock);
        let sup = Supervisor::start(f.clone());
        wait_running(dir.path(), "held").await;
        assert!(
            try_lock_plugin(dir.path(), "held").is_none(),
            "the lock is held while the plugin runs"
        );
        sup.stop().await;
        assert!(
            try_lock_plugin(dir.path(), "held").is_some(),
            "the lock is released once the plugin is stopped"
        );
    }
}
