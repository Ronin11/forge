//! The agent and the checks run under bubblewrap. Read-only system, private
//! /tmp, /run and /proc, a tmpfs $HOME with only the holes the attempt
//! needs: the task's clone (its .git included), the agent binary, and the
//! claude CLI's own state. Nothing else on the host is visible, and in
//! particular not the registered checkout or its .git. Network stays shared: the agent has to
//! reach the API.
//!
//! Sandboxing is on by default and refuses to run without bwrap unless
//! `FORGE2_SANDBOX=0` is set explicitly.

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Sandbox {
    bwrap: PathBuf,
    home: PathBuf,
    /// Directories holding the agent binary (as named and as resolved).
    agent_dirs: Vec<PathBuf>,
    /// Paths under $HOME the claude CLI must be able to write.
    write_paths: Vec<PathBuf>,
    /// Operator-configured toolchain paths, read-only.
    extra_ro: Vec<PathBuf>,
    /// Operator-configured package caches, read-write.
    extra_rw: Vec<PathBuf>,
}

/// Resolve a binary the way the shell would, then follow symlinks.
pub fn resolve_binary(name: &str) -> Result<(PathBuf, PathBuf)> {
    let named = if name.contains('/') {
        PathBuf::from(name)
    } else {
        std::env::var_os("PATH")
            .and_then(|p| {
                std::env::split_paths(&p)
                    .map(|d| d.join(name))
                    .find(|c| c.is_file())
            })
            .with_context(|| format!("{name} not found in PATH"))?
    };
    let canonical = named
        .canonicalize()
        .with_context(|| format!("resolving {}", named.display()))?;
    Ok((named, canonical))
}

impl Sandbox {
    /// `Ok(None)` only when the operator opted out with FORGE2_SANDBOX=0.
    pub fn detect(agent_bin: &str, paths: &crate::config::SandboxPaths) -> Result<Option<Sandbox>> {
        if std::env::var("FORGE2_SANDBOX").as_deref() == Ok("0") {
            return Ok(None);
        }
        let Ok((bwrap, _)) = resolve_binary("bwrap") else {
            bail!("bwrap not found; install bubblewrap or set FORGE2_SANDBOX=0 to run unsandboxed");
        };
        let home = PathBuf::from(std::env::var("HOME").context("HOME is not set")?);
        let mut agent_dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut bins = vec![agent_bin.to_string()];
        for (k, v) in std::env::vars() {
            if k.starts_with("FORGE2_CLAUDE_BIN_") {
                bins.push(v);
            }
        }
        for b in &bins {
            let (named, canonical) = resolve_binary(b)?;
            let real = PathBuf::from(crate::agent::real_bin(b));
            for p in [named, canonical, real] {
                if let Some(d) = p.parent() {
                    agent_dirs.insert(d.to_path_buf());
                }
            }
        }
        let config_dir = std::env::var("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".claude"));
        let write_paths = vec![config_dir, home.join(".claude.json")];
        Ok(Some(Sandbox {
            bwrap,
            home,
            agent_dirs: agent_dirs.into_iter().collect(),
            write_paths,
            extra_ro: {
                // Forge's own tools (forge-repomap) live beside the binary.
                let mut ro = paths.ro.clone();
                if let Ok(exe) = std::env::current_exe()
                    && let Some(dir) = exe.parent()
                {
                    ro.push(dir.to_path_buf());
                }
                ro
            },
            extra_rw: {
                // Shared caches (the repository map's parsed blobs) are
                // written from inside the sandbox.
                let mut rw = paths.rw.clone();
                if let Ok(p) = crate::ctx::Paths::resolve() {
                    let cache = p.home.join("cache");
                    let _ = std::fs::create_dir_all(&cache);
                    rw.push(cache);
                }
                rw
            },
        }))
    }

    /// Build the bwrap command that runs `argv` inside the worktree with
    /// exactly `env` (HOME is forced to the tmpfs home).
    pub fn command(&self, worktree: &Path, argv: &[String], env: &[(String, String)]) -> Command {
        let mut cmd = Command::new(&self.bwrap);
        cmd.args([
            "--die-with-parent",
            "--new-session",
            "--unshare-pid",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
        ]);
        cmd.args([
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind",
            "/etc",
            "/etc",
            "--ro-bind-try",
            "/opt",
            "/opt",
        ]);
        for p in ["/bin", "/sbin", "/lib", "/lib64", "/lib32"] {
            match std::fs::symlink_metadata(p) {
                Ok(m) if m.file_type().is_symlink() => {
                    if let Ok(target) = std::fs::read_link(p) {
                        cmd.arg("--symlink").arg(target).arg(p);
                    }
                }
                Ok(m) if m.is_dir() => {
                    cmd.args(["--ro-bind", p, p]);
                }
                _ => {}
            }
        }
        cmd.args(["--tmpfs", "/tmp", "--tmpfs", "/run"]);
        // systemd-resolved keeps the real resolv.conf under /run.
        cmd.args([
            "--ro-bind-try",
            "/run/systemd/resolve",
            "/run/systemd/resolve",
        ]);
        cmd.arg("--tmpfs").arg(&self.home);
        // Order matters: everything under $HOME is bound after its tmpfs, and
        // the writable worktree after the read-only agent directory in case
        // one contains the other.
        for d in self.agent_dirs.iter().chain(&self.extra_ro) {
            cmd.arg("--ro-bind-try").arg(d).arg(d);
        }
        cmd.arg("--bind").arg(worktree).arg(worktree);
        for p in self.write_paths.iter().chain(&self.extra_rw) {
            cmd.arg("--bind-try").arg(p).arg(p);
        }
        cmd.arg("--chdir").arg(worktree).arg("--");
        cmd.args(argv);
        cmd.env_clear();
        cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd.env("HOME", &self.home);
        cmd
    }
}
