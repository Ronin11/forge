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

/// `$XDG_CONFIG_HOME/systemd/user`, else `$HOME/.config/systemd/user` —
/// the OS user's own config directory, never `FORGE_HOME` (which may sit
/// elsewhere entirely): systemd only ever looks for user units there.
pub(crate) fn systemd_user_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(p).join("systemd/user"));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".config/systemd/user"))
}

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

fn worker_unit(home: &Path, forge_bin: &Path, bin_dir: &Path) -> String {
    format!(
        "# Written by `forge init`; re-run it after moving the binary.\n\
[Unit]\n\
Description=Forge worker\n\
After=network-online.target\n\
\n\
[Service]\n\
Type=simple\n\
Environment=FORGE_HOME={home}\n\
Environment=PATH={bin_dir}:/usr/local/bin:/usr/bin:/bin\n\
ExecStart={forge_bin} work --jobs 4\n\
KillSignal=SIGTERM\n\
KillMode=mixed\n\
TimeoutStopSec=2400\n\
Restart=on-failure\n\
RestartSec=10\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        home = home.display(),
        bin_dir = bin_dir.display(),
        forge_bin = forge_bin.display(),
    )
}

fn web_unit(home: &Path, forge_bin: &Path, web_bin: &Path, bin_dir: &Path) -> String {
    format!(
        "# Written by `forge init`; re-run it after moving the binary.\n\
[Unit]\n\
Description=Forge web client\n\
After=network.target\n\
\n\
[Service]\n\
Environment=FORGE_HOME={home}\n\
Environment=FORGE_BIN={forge_bin}\n\
Environment=PATH={bin_dir}:/usr/local/bin:/usr/bin:/bin\n\
ExecStart={web_bin} --bind 127.0.0.1:7788\n\
Restart=on-failure\n\
RestartSec=3\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        home = home.display(),
        bin_dir = bin_dir.display(),
        forge_bin = forge_bin.display(),
        web_bin = web_bin.display(),
    )
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

/// The two unit files, written under the OS user's systemd config
/// directory with the currently running binary's own path, enabled and
/// started with linger when a systemd user session is reachable; when it
/// is not, the files are still written (so they are ready once systemd
/// is), and the commands the operator would run by hand are printed in
/// the returned detail instead of being run.
fn install_units(home: &Path) -> Result<StepResult> {
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
    let worker_changed = write_if_changed(&worker_path, &worker_unit(home, &forge_bin, &bin_dir))?;
    let web_changed = write_if_changed(&web_path, &web_unit(home, &forge_bin, &web_bin, &bin_dir))?;
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

/// `--relink`: move the running install onto the release layout — copy the
/// running binaries into `releases/<commit>/` and point `current` at it,
/// unless `current` already exists. Never touches a live release.
fn adopt_running_binaries(home: &Path) -> Result<Vec<StepResult>> {
    let root = release::root(home);
    let exe = crate::binary::without_deleted_suffix(&std::env::current_exe()?);
    let src = exe
        .parent()
        .context("the running binary has no directory")?;
    let sha = env!("FORGE_GIT_SHA");
    let id = if sha.is_empty() {
        env!("CARGO_PKG_VERSION")
    } else {
        sha
    };
    let mut steps = Vec::new();
    match release::pointed_at(&root, "current") {
        Some(live) => steps.push(step("release", false, format!("current is already {live}"))),
        None => {
            let made = release::install(&root, src, id)?;
            release::flip(&root, id)?;
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

/// Everything `forge init` does, in order. `home_override` is `--home`;
/// `None` uses the usual resolution (`Paths::compute_home`).
pub async fn run(home_override: Option<PathBuf>, relink: bool) -> Result<Report> {
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
    let commit = git::commit_all(&catalog, "forge init: built-in workflow catalog").await?;
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
    steps.push(install_units(&home)?);

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
}
