//! `forge init [--home DIR]`: everything a fresh machine needs before
//! `forge work` or `forge-web` can run — the data directory, the
//! operator's config template, the workflow catalog as a committed git
//! repository, `web.token`, and (when systemd is available) the worker
//! and web user units, enabled with linger. On a platform without systemd
//! (macOS) it writes no units and prints the two commands that run the
//! worker and web client by hand. Idempotent: run again and every step
//! reports nothing changed.

use crate::{config, ctx::Paths, git, release, workflows};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// One step `forge init` took (or found already done).
pub struct StepResult {
    pub name: &'static str,
    pub changed: bool,
    pub detail: String,
}

fn step(name: &'static str, changed: bool, detail: impl Into<String>) -> StepResult {
    StepResult {
        name,
        changed,
        detail: detail.into(),
    }
}

pub struct Report {
    pub home: PathBuf,
    pub steps: Vec<StepResult>,
}

impl Report {
    pub fn changed_anything(&self) -> bool {
        self.steps.iter().any(|s| s.changed)
    }
}

pub(crate) use crate::unit_path::systemd_user_dir;

/// `/run` in production; `tests/e2e/init.rs`'s session-path test points
/// this at a tempdir holding its own `systemd/system` marker, since the
/// real one can only be produced by actually booting under systemd — the
/// one piece of `systemd_available` a test can't otherwise fake.
fn systemd_run_dir() -> PathBuf {
    std::env::var_os("FORGE_TEST_SYSTEMD_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run"))
}

/// Whether a systemd user session is reachable at all: `sd_booted()`'s own
/// check (`<run>/systemd/system`, set only once systemd is pid 1) plus
/// `XDG_RUNTIME_DIR`, which a login session sets and `systemctl --user`
/// needs to find the session bus. Both are plain reads, so detecting "no
/// systemd" (every sandbox and most CI containers) never spawns a process
/// or touches disk.
fn systemd_available() -> bool {
    systemd_run_dir().join("systemd/system").exists()
        && std::env::var_os("XDG_RUNTIME_DIR").is_some()
}

use crate::unit_path::{environment_line as env_line, exec_start_line as exec_line};

fn display(p: &Path) -> String {
    p.display().to_string()
}

fn worker_unit(home: &Path, forge_bin: &Path, path: &str) -> Result<String> {
    Ok(format!(
        "# Written by `forge init`; re-run it after moving the binary.\n\
[Unit]\n\
Description=Forge worker\n\
After=network-online.target\n\
StartLimitIntervalSec=10800\n\
StartLimitBurst=5\n\
\n\
[Service]\n\
Type=notify\n\
NotifyAccess=all\n\
{home}\n\
{path}\n\
{exec}\n\
KillSignal=SIGTERM\n\
KillMode=mixed\n\
TimeoutStopSec=2400\n\
Restart=on-failure\n\
RestartSec=10\n\
RestartSteps=5\n\
RestartMaxDelaySec=900\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        home = env_line("FORGE_HOME", &display(home))?,
        path = env_line("PATH", path)?,
        exec = exec_line(&[&display(forge_bin), "work"])?,
    ))
}

fn web_unit(home: &Path, forge_bin: &Path, web_bin: &Path, path: &str) -> Result<String> {
    Ok(format!(
        "# Written by `forge init`; re-run it after moving the binary.\n\
[Unit]\n\
Description=Forge web client\n\
After=network.target\n\
\n\
[Service]\n\
{home}\n\
{forge_bin}\n\
{path}\n\
{exec}\n\
Restart=on-failure\n\
RestartSec=3\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        home = env_line("FORGE_HOME", &display(home))?,
        forge_bin = env_line("FORGE_BIN", &display(forge_bin))?,
        path = env_line("PATH", path)?,
        exec = exec_line(&[&display(web_bin), "--bind", "127.0.0.1:7788"])?,
    ))
}

/// Write `path` only when its content would change, so a second run
/// reports no change.
fn write_if_changed(path: &Path, content: &str) -> Result<bool> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(content) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)?;
    Ok(true)
}

/// The two commands that run the worker and the web client by hand, for a
/// machine with no systemd at all (macOS): nothing is written under
/// `~/.config/systemd`, which need not exist there.
fn by_hand(home: &Path, forge_bin: &Path, web_bin: &Path) -> StepResult {
    step(
        "systemd",
        false,
        format!(
            "no systemd on this platform; run the worker and web client by hand:\n  \
             FORGE_HOME={home} {forge_bin} work\n  \
             FORGE_HOME={home} FORGE_BIN={forge_bin} {web_bin} --bind 127.0.0.1:7788",
            home = crate::sandbox::shell_quote(&home.display().to_string()),
            forge_bin = crate::sandbox::shell_quote(&forge_bin.display().to_string()),
            web_bin = crate::sandbox::shell_quote(&web_bin.display().to_string()),
        ),
    )
}

/// The `forge` the units run and the directory it sits in.
fn forge_binary(home: &Path) -> Result<(PathBuf, PathBuf)> {
    let current = release::root(home).join("current");
    let forge_bin = if current.join("forge").is_file() {
        current.join("forge")
    } else {
        crate::binary::without_deleted_suffix(&std::env::current_exe()?)
    };
    let bin_dir = forge_bin
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok((forge_bin, bin_dir))
}

/// The PATH the units declare: `bin_dir`, then this shell's own PATH, so
/// the agent CLIs the operator can run are on the worker's PATH too.
fn unit_path(bin_dir: &Path) -> String {
    crate::unit_path::compose(bin_dir, std::env::var_os("PATH"))
}

/// The PATH to write into the unit at `unit`: this shell's, then whatever
/// the unit's own `Environment=PATH=` already had that this shell lacks —
/// unless `reset`, which writes this shell's alone.
fn path_for_unit(unit: &Path, bin_dir: &Path, reset: bool) -> String {
    let fresh = unit_path(bin_dir);
    if reset {
        return fresh;
    }
    crate::unit_path::merge(&fresh, crate::unit_path::declared_path(unit).as_deref())
}

/// Split a `systemctl show` `Environment=` value into its assignments:
/// whitespace separates them, and a double-quoted assignment (one holding a
/// space) is a single word with `\"` and `\\` escapes.
fn split_assignments(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut escaped, mut any) = (false, false, false);
    for c in value.chars() {
        match c {
            _ if escaped => {
                cur.push(c);
                escaped = false;
            }
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    words.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(cur);
    }
    words
}

/// The PATH of a running process from its `/proc/<pid>/environ` bytes.
fn path_from_environ(environ: &[u8]) -> Option<String> {
    environ
        .split(|b| *b == 0)
        .find_map(|kv| kv.strip_prefix(b"PATH="))
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

/// The PATH the worker is running with, from `systemctl --user show`
/// output asking for `MainPID` and `Environment`: the live process's own
/// environment when there is one, else the PATH systemd has loaded for the
/// unit. `None` when the unit is not running and declares none.
fn live_path_from_show(show: &str, environ_of: impl Fn(u32) -> Option<Vec<u8>>) -> Option<String> {
    let value = |key: &str| {
        show.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
    };
    let running = value("MainPID")
        .and_then(|p| p.trim().parse::<u32>().ok())
        .filter(|p| *p > 0)
        .and_then(environ_of)
        .and_then(|e| path_from_environ(&e));
    running.or_else(|| {
        split_assignments(value("Environment")?)
            .into_iter()
            .find_map(|w| w.strip_prefix("PATH=").map(str::to_string))
    })
}

/// What the running worker's PATH is, asked of systemd before anything is
/// reloaded (a reload replaces the loaded environment with the new file's).
fn live_worker_path() -> Option<String> {
    let o = std::process::Command::new("systemctl")
        .args(["--user", "show", crate::unit_path::WORKER_UNIT])
        .args(["-p", "MainPID", "-p", "Environment"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    live_path_from_show(&String::from_utf8_lossy(&o.stdout), |pid| {
        std::fs::read(format!("/proc/{pid}/environ")).ok()
    })
}

/// Whether systemd reports the unit called `name` enabled. By name, not
/// path: `is-enabled` refuses a unit file path with "Invalid argument".
fn is_enabled(name: &str) -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-enabled", name])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The lines that say a changed unit is not yet running: only a restart
/// applies it (`enable --now` leaves an active unit alone), and the
/// worker's live PATH when it is not the one just written.
fn restart_notes(changed: &[&str], worker_live_path: Option<&str>, worker_path: &str) -> String {
    let mut notes = String::new();
    for unit in changed {
        notes.push_str(&format!(
            "\n  restart {unit} to apply: systemctl --user restart {unit}.service"
        ));
        if *unit == "forge-worker"
            && let Some(live) = worker_live_path.filter(|l| *l != worker_path)
        {
            notes.push_str(&format!(
                "\n  the running worker's PATH is {live}\n  the unit's PATH is now {worker_path}"
            ));
        }
    }
    notes
}

/// The two unit files, written under the OS user's systemd config
/// directory with the currently running binary's own path, enabled and
/// started with linger when a systemd user session is reachable; when it
/// is not, the files are still written (so they are ready once systemd
/// is), and the commands the operator would run by hand are printed in
/// the returned detail instead of being run. Each unit is asked
/// `is-enabled` and enabled when it is not; a changed unit is written but
/// not applied to a running process, and the detail says to restart it.
fn install_units(home: &Path, reset_path: bool) -> Result<StepResult> {
    let (forge_bin, bin_dir) = forge_binary(home)?;
    let web_bin = bin_dir.join("forge-web");
    if !cfg!(target_os = "linux") {
        return Ok(by_hand(home, &forge_bin, &web_bin));
    }
    let Some(dir) = systemd_user_dir() else {
        return Ok(step(
            "systemd",
            false,
            "HOME is not set; cannot locate ~/.config/systemd/user",
        ));
    };
    let worker_path = dir.join("forge-worker.service");
    let web_path = dir.join("forge-web.service");
    let worker_unit_path = path_for_unit(&worker_path, &bin_dir, reset_path);
    let web_unit_path = path_for_unit(&web_path, &bin_dir, reset_path);
    let worker_changed = write_if_changed(
        &worker_path,
        &worker_unit(home, &forge_bin, &worker_unit_path)?,
    )?;
    let web_changed = write_if_changed(
        &web_path,
        &web_unit(home, &forge_bin, &web_bin, &web_unit_path)?,
    )?;
    let files_changed = worker_changed || web_changed;

    if !systemd_available() {
        return Ok(step(
            "systemd",
            files_changed,
            format!(
                "wrote {worker} and {web}; no systemd user session detected, so run by hand once one is available:\n  \
                 systemctl --user daemon-reload\n  \
                 systemctl --user enable --now {worker} {web}\n  \
                 loginctl enable-linger",
                worker = worker_path.display(),
                web = web_path.display(),
            ),
        ));
    }

    let live_path = if worker_changed {
        live_worker_path()
    } else {
        None
    };
    let sh = |args: &[&str]| -> Result<()> {
        let o = std::process::Command::new(args[0])
            .args(&args[1..])
            .output()?;
        anyhow::ensure!(
            o.status.success(),
            "{} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        );
        Ok(())
    };
    if files_changed {
        sh(&["systemctl", "--user", "daemon-reload"])?;
    }
    let disabled: Vec<&Path> = [
        (crate::unit_path::WORKER_UNIT, worker_path.as_path()),
        (crate::unit_path::WEB_UNIT, web_path.as_path()),
    ]
    .into_iter()
    .filter(|(name, _)| !is_enabled(name))
    .map(|(_, path)| path)
    .collect();
    if !disabled.is_empty() {
        // The unit file paths this run wrote, not bare unit names: a bare
        // "forge-worker.service" would let systemctl's own search path
        // resolve to some other unit of the same name (the operator's real
        // one, if `XDG_CONFIG_HOME` here is a test's own tempdir rather
        // than the OS user's actual config directory).
        let paths: Vec<String> = disabled
            .iter()
            .map(|u| u.to_string_lossy().into())
            .collect();
        let mut args = vec!["systemctl", "--user", "enable", "--now"];
        args.extend(paths.iter().map(String::as_str));
        sh(&args)?;
        sh(&["loginctl", "enable-linger"])?;
    }

    let mut changed_units = Vec::new();
    if worker_changed {
        changed_units.push("forge-worker");
    }
    if web_changed {
        changed_units.push("forge-web");
    }
    let notes = restart_notes(&changed_units, live_path.as_deref(), &worker_unit_path);
    let detail = match (disabled.is_empty(), files_changed) {
        (true, false) => format!(
            "{} and {} already installed and enabled",
            worker_path.display(),
            web_path.display()
        ),
        (true, true) => format!(
            "wrote {} and {}, already enabled{notes}",
            worker_path.display(),
            web_path.display()
        ),
        (false, _) => format!(
            "installed and enabled {} and {}, with linger{notes}",
            worker_path.display(),
            web_path.display()
        ),
    };
    Ok(step(
        "systemd",
        files_changed || !disabled.is_empty(),
        detail,
    ))
}

/// `$HOME/.local/bin`, where the operator's own PATH finds `forge`.
fn local_bin_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin"))
}

/// The binaries of `release::BINS` a release adopted from `dir` needs but
/// `dir` lacks: every one but `forge-test`, which `deploy-self` too treats
/// as optional.
fn missing_release_bins(dir: &Path) -> Vec<&'static str> {
    release::BINS
        .iter()
        .copied()
        .filter(|b| *b != "forge-test" && !dir.join(b).is_file())
        .collect()
}

/// `--relink`: move the running install onto the release layout — copy the
/// running binaries into `releases/<full commit>/` (the id `deploy-self`
/// gives the same commit) and point `current` at it, unless `current`
/// already exists. Refuses a directory missing any release binary, so a
/// fresh `forge` beside stale siblings never becomes a release. Never
/// touches a live release.
fn adopt_running_binaries(home: &Path) -> Result<Vec<StepResult>> {
    let root = release::root(home);
    let exe = crate::binary::without_deleted_suffix(&std::env::current_exe()?);
    let src = exe
        .parent()
        .context("the running binary has no directory")?;
    let sha = env!("FORGE_GIT_SHA_FULL");
    let id = if sha.is_empty() {
        env!("CARGO_PKG_VERSION")
    } else {
        sha
    };
    let mut steps = Vec::new();
    let lock = release::lock(&root)?;
    match release::pointed_at(&root, "current") {
        Some(live) => steps.push(step("release", false, format!("current is already {live}"))),
        None => {
            let missing = missing_release_bins(src);
            anyhow::ensure!(
                missing.is_empty(),
                "--relink: {} lacks {}; build the whole workspace (cargo build --release --workspace) and run its forge",
                src.display(),
                missing.join(", ")
            );
            let made = release::install(&lock, &root, src, id)?;
            release::drop_staged(&lock, &root)?;
            release::flip(&lock, &root, id)?;
            let detail = format!(
                "copied {} into {} and pointed {} at it{}",
                src.display(),
                release::release_dir(&root, id).display(),
                root.join("current").display(),
                if made {
                    ""
                } else {
                    " (release already present)"
                }
            );
            steps.push(step("release", true, detail));
        }
    }
    Ok(steps)
}

/// `~/.local/bin/<name>` -> `FORGE_HOME/bin/current/<name>` for every
/// binary the live release holds. Runs only once `current` exists.
fn link_local_bin(home: &Path) -> Result<Option<StepResult>> {
    let current = release::root(home).join("current");
    let Some(dir) = local_bin_dir().filter(|_| current.is_dir()) else {
        return Ok(None);
    };
    let mut changed = Vec::new();
    for b in release::BINS {
        if current.join(b).is_file() && release::relink(&dir.join(b), &current.join(b))? {
            changed.push(dir.join(b).display().to_string());
        }
    }
    let detail = if changed.is_empty() {
        format!("{} already through {}", dir.display(), current.display())
    } else {
        format!(
            "linked {} through {}",
            changed.join(", "),
            current.display()
        )
    };
    Ok(Some(step("links", !changed.is_empty(), detail)))
}

/// The bare origin's post-update hook, as reviewed: mirrors `main` and
/// `v*` tags to `git config forge.mirror` in the background, bounded, and
/// never fails the push (see the file's own header).
pub const MIRROR_HOOK: &str = include_str!("../deploy/post-update.mirror");

/// The remote a registered repository pushes to (`[defaults] remote`,
/// `origin` when its config cannot be read), or `None` for `push = false`.
async fn origin_remote(repo: &Path) -> Option<String> {
    match config::load_working(repo).await {
        Ok(cfg) => cfg.push_remote,
        Err(_) => Some("origin".to_string()),
    }
}

/// The bare repository a remote URL names on this machine, if it is one:
/// a network URL (or a missing path) is somebody else's to hook.
async fn local_bare(url: &str) -> Option<PathBuf> {
    let path = PathBuf::from(url.strip_prefix("file://").unwrap_or(url));
    if !path.is_absolute() || !path.is_dir() {
        return None;
    }
    if !git::is_bare(&path).await {
        return None;
    }
    Some(path.canonicalize().unwrap_or(path))
}

/// Install `MIRROR_HOOK` as `bare`'s post-update hook (honoring its
/// `core.hooksPath`) and point it at `mirror`. A different hook already
/// there is kept once as `post-update.before-forge`. Returns whether
/// anything changed.
async fn install_mirror_hook(bare: &Path, mirror: &str) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let hooks = git::hooks_dir(bare).await?;
    let hook = hooks.join("post-update");
    let mut changed = false;
    let old = std::fs::read_to_string(&hook).ok();
    if old.as_deref() != Some(MIRROR_HOOK) {
        let backup = hooks.join("post-update.before-forge");
        if old.is_some() && !backup.exists() {
            std::fs::rename(&hook, &backup)?;
        }
        write_if_changed(&hook, MIRROR_HOOK)?;
        changed = true;
    }
    let mode = std::fs::metadata(&hook)?.permissions().mode();
    if mode & 0o111 != 0o111 {
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(mode | 0o755))?;
        changed = true;
    }
    if git::config_get(bare, "forge.mirror").await.as_deref() != Some(mirror) {
        git::config_set(bare, "forge.mirror", mirror).await?;
        changed = true;
    }
    Ok(changed)
}

/// `--mirror <remote>`: the mirror hook in the bare origin of every
/// registered repository whose push remote is a bare repository on this
/// machine. None found is an error: the operator asked for a mirror.
async fn install_mirrors(home: &Path, mirror: &str) -> Result<Vec<StepResult>> {
    let store = crate::store::Store::open(&home.join("forge.db"))?;
    let mut bares = std::collections::BTreeSet::new();
    for p in store.list_projects()? {
        for r in store.project_repos(&p.name)? {
            let repo = Path::new(&r.repo);
            let Some(remote) = origin_remote(repo).await else {
                continue;
            };
            let Some(url) = git::remote_url(repo, &remote).await else {
                continue;
            };
            if let Some(bare) = local_bare(&url).await {
                bares.insert(bare);
            }
        }
    }
    anyhow::ensure!(
        !bares.is_empty(),
        "--mirror {mirror}: no registered repository has a bare origin on this machine"
    );
    let mut steps = Vec::new();
    for bare in bares {
        let changed = install_mirror_hook(&bare, mirror)
            .await
            .with_context(|| format!("installing the mirror hook in {}", bare.display()))?;
        let detail = format!(
            "{} post-update mirrors main and v* tags to {mirror}",
            bare.display()
        );
        steps.push(step("mirror", changed, detail));
    }
    Ok(steps)
}

/// Commit the catalog files `workflows::catalog_dir` wrote (the built-in
/// workflows and the untrusted-data fragment): those the repository does not
/// track yet and whose text is exactly what this binary writes. Whatever else
/// sits in the catalog, an operator's uncommitted edit above all, is left as
/// it is.
async fn commit_written_catalog_files(catalog: &Path) -> Result<Option<String>> {
    let written = workflows::BUILTIN_WORKFLOWS
        .iter()
        .map(|(file, text)| (file.to_string(), *text))
        .chain([(
            format!("{}/untrusted-data.md", workflows::FRAGMENTS_DIR),
            workflows::UNTRUSTED_DATA,
        )]);
    let mut paths = Vec::new();
    for (rel, text) in written {
        let tracked = std::process::Command::new("git")
            .arg("-C")
            .arg(catalog)
            .args(["ls-files", "--error-unmatch", "--", &rel])
            .output()?
            .status
            .success();
        if !tracked && std::fs::read_to_string(catalog.join(&rel)).is_ok_and(|t| t == text) {
            paths.push(rel);
        }
    }
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    git::commit_paths(catalog, &paths, "forge init: built-in workflow catalog").await
}

/// Everything `forge init` does, in order. `home_override` is `--home`;
/// `None` uses the usual resolution (`Paths::compute_home`). `mirror` is
/// `--mirror`: install the mirror hook into the registered bare origins.
pub async fn run(
    home_override: Option<PathBuf>,
    relink: bool,
    mirror: Option<&str>,
    reset_path: bool,
) -> Result<Report> {
    let home = match home_override {
        Some(h) => h,
        None => Paths::compute_home()?,
    };
    let mut steps = Vec::new();

    let existed = home.exists();
    let paths = Paths::for_home(home.clone())?;
    steps.push(step("home", !existed, paths.home.display().to_string()));

    let config_path = home.join("config.toml");
    let had_config = config_path.exists();
    config::ensure_home_config(&home)?;
    steps.push(step(
        "config",
        !had_config,
        config_path.display().to_string(),
    ));

    let catalog = workflows::catalog_dir(&home)?;
    let commit = commit_written_catalog_files(&catalog).await?;
    steps.push(match commit {
        Some(sha) => step(
            "workflows",
            true,
            format!("{} committed built-ins as {sha}", catalog.display()),
        ),
        None => step(
            "workflows",
            false,
            format!("{} already committed", catalog.display()),
        ),
    });

    let token_path = home.join("web.token");
    let had_token = token_path.exists();
    crate::cli::web_token(&home)?;
    steps.push(step(
        "web.token",
        !had_token,
        token_path.display().to_string(),
    ));

    if relink {
        steps.extend(adopt_running_binaries(&home)?);
    }
    steps.extend(link_local_bin(&home)?);
    if let Some(m) = mirror {
        steps.extend(install_mirrors(&home, m).await?);
    }
    steps.push(install_units(&home, reset_path)?);
    let (_, bin_dir) = forge_binary(&home)?;
    let worker = systemd_user_dir().map(|d| d.join(crate::unit_path::WORKER_UNIT));
    let shown = match worker {
        Some(w) => crate::unit_path::declared_path(&w).unwrap_or_else(|| unit_path(&bin_dir)),
        None => unit_path(&bin_dir),
    };
    steps.push(step("unit PATH", false, shown));

    Ok(Report { home, steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_hand_names_the_worker_and_web_commands_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("forge home");
        let s = by_hand(
            &home,
            Path::new("/opt/forge/forge"),
            Path::new("/opt/forge/forge-web"),
        );
        assert!(!s.changed);
        let lines: Vec<&str> = s.detail.lines().map(str::trim).collect();
        let quoted = format!("'{}'", home.display());
        assert_eq!(
            lines[1],
            format!("FORGE_HOME={quoted} '/opt/forge/forge' work")
        );
        assert_eq!(
            lines[2],
            format!(
                "FORGE_HOME={quoted} FORGE_BIN='/opt/forge/forge' '/opt/forge/forge-web' --bind 127.0.0.1:7788"
            )
        );
        assert!(!home.exists());
    }

    #[test]
    fn a_directory_with_only_forge_is_missing_every_other_release_binary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("forge"), "").unwrap();
        assert_eq!(
            missing_release_bins(dir.path()),
            ["forge-web", "forge-portal", "forge-repomap", "forge-tui"]
        );
        for b in release::BINS.iter().filter(|b| **b != "forge-test") {
            std::fs::write(dir.path().join(b), "").unwrap();
        }
        assert!(missing_release_bins(dir.path()).is_empty());
    }

    #[test]
    fn path_for_unit_keeps_the_units_own_entries_unless_reset() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join("w.service");
        std::fs::write(
            &unit,
            "[Service]\nEnvironment=PATH=/h/bin:/opt/agents:/usr/bin\n",
        )
        .unwrap();
        let fresh = unit_path(Path::new("/h/bin"));
        let kept = path_for_unit(&unit, Path::new("/h/bin"), false);
        assert_eq!(
            kept,
            crate::unit_path::merge(&fresh, Some("/h/bin:/opt/agents:/usr/bin"))
        );
        assert!(kept.starts_with(&fresh), "{kept}");
        assert!(kept.split(':').any(|d| d == "/opt/agents"), "{kept}");
        assert_eq!(path_for_unit(&unit, Path::new("/h/bin"), true), fresh);
        let missing = dir.path().join("none.service");
        assert_eq!(path_for_unit(&missing, Path::new("/h/bin"), false), fresh);
    }

    #[test]
    fn split_assignments_honours_quotes() {
        assert_eq!(
            split_assignments(r#"FORGE_HOME=/h "PATH=/a b:/c" X=1"#),
            ["FORGE_HOME=/h", "PATH=/a b:/c", "X=1"]
        );
        assert!(split_assignments("").is_empty());
    }

    #[test]
    fn live_path_prefers_the_running_process_then_the_loaded_environment() {
        let show = "MainPID=42\nEnvironment=FORGE_HOME=/h PATH=/loaded\n";
        let environ = |pid: u32| {
            assert_eq!(pid, 42);
            Some(b"HOME=/x\0PATH=/live:/bin\0".to_vec())
        };
        assert_eq!(
            live_path_from_show(show, environ).as_deref(),
            Some("/live:/bin")
        );
        assert_eq!(
            live_path_from_show(show, |_| None).as_deref(),
            Some("/loaded")
        );
        let stopped = "MainPID=0\nEnvironment=FORGE_HOME=/h\n";
        assert_eq!(live_path_from_show(stopped, |_| None), None);
    }

    #[test]
    fn restart_notes_say_restart_and_name_a_differing_live_path() {
        let n = restart_notes(&["forge-worker"], Some("/old"), "/new");
        assert!(n.contains("restart forge-worker to apply"), "{n}");
        assert!(n.contains("running worker's PATH is /old"), "{n}");
        let same = restart_notes(&["forge-worker"], Some("/new"), "/new");
        assert!(same.contains("restart forge-worker to apply"), "{same}");
        assert!(!same.contains("running worker's PATH"), "{same}");
        assert!(restart_notes(&[], Some("/old"), "/new").is_empty());
    }
}
