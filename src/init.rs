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
        exec = exec_line(&[&display(forge_bin), "work", "--jobs", "4"])?,
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
             FORGE_HOME={home} {forge_bin} work --jobs 4\n  \
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

/// The two unit files, written under the OS user's systemd config
/// directory with the currently running binary's own path, enabled and
/// started with linger when a systemd user session is reachable; when it
/// is not, the files are still written (so they are ready once systemd
/// is), and the commands the operator would run by hand are printed in
/// the returned detail instead of being run.
fn install_units(home: &Path) -> Result<StepResult> {
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
    let path = unit_path(&bin_dir);
    let worker_changed = write_if_changed(&worker_path, &worker_unit(home, &forge_bin, &path)?)?;
    let web_changed = write_if_changed(&web_path, &web_unit(home, &forge_bin, &web_bin, &path)?)?;
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

    if !files_changed {
        return Ok(step(
            "systemd",
            false,
            format!(
                "{} and {} already installed and enabled",
                worker_path.display(),
                web_path.display()
            ),
        ));
    }

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
    let worker_path_str = worker_path.to_string_lossy();
    let web_path_str = web_path.to_string_lossy();
    sh(&["systemctl", "--user", "daemon-reload"])?;
    // The unit file paths this run just wrote, not bare unit names: a
    // bare "forge-worker.service" would let systemctl's own search path
    // resolve to some other unit of the same name (the operator's real
    // one, if `XDG_CONFIG_HOME` here is a test's own tempdir rather than
    // the OS user's actual config directory).
    sh(&[
        "systemctl",
        "--user",
        "enable",
        "--now",
        &worker_path_str,
        &web_path_str,
    ])?;
    sh(&["loginctl", "enable-linger"])?;

    Ok(step(
        "systemd",
        true,
        format!(
            "installed and enabled {} and {}, with linger",
            worker_path.display(),
            web_path.display()
        ),
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
    steps.push(install_units(&home)?);
    let (_, bin_dir) = forge_binary(&home)?;
    steps.push(step("unit PATH", false, unit_path(&bin_dir)));

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
            format!("FORGE_HOME={quoted} '/opt/forge/forge' work --jobs 4")
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
}
