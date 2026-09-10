//! The agent runs under bubblewrap. Read-only system, private /tmp, /run
//! and /proc, a tmpfs $HOME with only the holes the attempt needs: the
//! worktree, the repository's .git (a worktree cannot commit without it),
//! the agent binary, and the claude CLI's own state. Nothing else on the
//! host is visible. Network stays shared: the agent has to reach the API.
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
    pub fn detect(agent_bin: &str) -> Result<Option<Sandbox>> {
        if std::env::var("FORGE2_SANDBOX").as_deref() == Ok("0") {
            eprintln!("sandbox  OFF (FORGE2_SANDBOX=0): the agent runs directly on the host");
            return Ok(None);
        }
        let Ok((bwrap, _)) = resolve_binary("bwrap") else {
            bail!("bwrap not found; install bubblewrap or set FORGE2_SANDBOX=0 to run unsandboxed");
        };
        let home = PathBuf::from(std::env::var("HOME").context("HOME is not set")?);
        let (named, canonical) = resolve_binary(agent_bin)?;
        let mut agent_dirs: BTreeSet<PathBuf> = BTreeSet::new();
        for p in [named, canonical] {
            if let Some(d) = p.parent() {
                agent_dirs.insert(d.to_path_buf());
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
        }))
    }

    /// Build the bwrap command that runs `argv` inside the worktree.
    pub fn command(&self, worktree: &Path, repo_git_dir: &Path, argv: &[String]) -> Command {
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
        for d in &self.agent_dirs {
            cmd.arg("--ro-bind-try").arg(d).arg(d);
        }
        cmd.arg("--bind").arg(worktree).arg(worktree);
        cmd.arg("--bind-try").arg(repo_git_dir).arg(repo_git_dir);
        for p in &self.write_paths {
            cmd.arg("--bind-try").arg(p).arg(p);
        }
        cmd.arg("--chdir").arg(worktree).arg("--");
        cmd.args(argv);
        cmd.env_clear();
        for (k, v) in std::env::vars() {
            let keep = matches!(k.as_str(), "PATH" | "LANG" | "TERM" | "CLAUDE_CONFIG_DIR")
                || ["LC_", "ANTHROPIC_"].iter().any(|p| k.starts_with(p));
            if keep {
                cmd.env(k, v);
            }
        }
        cmd.env("HOME", &self.home);
        cmd
    }
}
