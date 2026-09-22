//! `forge init [--home DIR]`: everything a fresh machine needs before
//! `forge work` or `forge-web` can run — the data directory, the
//! operator's config template, the workflow catalog as a committed git
//! repository, `web.token`, and (when systemd is available) the worker
//! and web user units, enabled with linger. Idempotent: run again and
//! every step reports nothing changed.

use crate::{config, ctx::Paths, git, workflows};
use anyhow::Result;
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

/// Whether a systemd user session is reachable at all: `sd_booted()`'s own
/// check (`/run/systemd/system`, set only once systemd is pid 1) plus
/// `XDG_RUNTIME_DIR`, which a login session sets and `systemctl --user`
/// needs to find the session bus. Both are plain reads, so detecting "no
/// systemd" (every sandbox and most CI containers) never spawns a process
/// or touches disk.
fn systemd_available() -> bool {
    Path::new("/run/systemd/system").exists() && std::env::var_os("XDG_RUNTIME_DIR").is_some()
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

/// The two unit files, written under the OS user's systemd config
/// directory with the currently running binary's own path, enabled and
/// started with linger when a systemd user session is reachable; when it
/// is not, the files are still written (so they are ready once systemd
/// is), and the commands the operator would run by hand are printed in
/// the returned detail instead of being run.
fn install_units(home: &Path) -> Result<StepResult> {
    let Some(dir) = systemd_user_dir() else {
        return Ok(step(
            "systemd",
            false,
            "HOME is not set; cannot locate ~/.config/systemd/user",
        ));
    };
    let forge_bin = std::env::current_exe()?;
    let bin_dir = forge_bin
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let web_bin = bin_dir.join("forge-web");

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
                "wrote {} and {}; no systemd user session detected, so run by hand once one is available:\n  \
                 systemctl --user daemon-reload\n  \
                 systemctl --user enable --now forge-worker.service forge-web.service\n  \
                 loginctl enable-linger",
                worker_path.display(),
                web_path.display(),
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
    sh(&["systemctl", "--user", "daemon-reload"])?;
    sh(&[
        "systemctl",
        "--user",
        "enable",
        "--now",
        "forge-worker.service",
        "forge-web.service",
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

/// Everything `forge init` does, in order. `home_override` is `--home`;
/// `None` uses the usual resolution (`Paths::compute_home`).
pub async fn run(home_override: Option<PathBuf>) -> Result<Report> {
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

    steps.push(install_units(&home)?);

    Ok(Report { home, steps })
}
