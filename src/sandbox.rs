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

/// Where the host's `~/.claude.json` is bound read-only inside the sandbox.
/// Not under `/opt`: on a host with a real `/opt`, that directory is itself
/// bound read-only a few flags earlier, and bwrap cannot create the
/// `forge/seed` path inside a read-only mount to bind onto. `/run` is a
/// tmpfs bwrap creates itself, so it always has room.
const CLAUDE_JSON_SEED: &str = "/run/forge/seed/claude.json";

pub struct Sandbox {
    bwrap: PathBuf,
    home: PathBuf,
    /// Directories holding the agent binary (as named and as resolved).
    agent_dirs: Vec<PathBuf>,
    /// Paths under $HOME the claude CLI must be able to write: today just
    /// the config directory, where credentials live.
    write_paths: Vec<PathBuf>,
    /// The host's `~/.claude.json`, bound read-only at `CLAUDE_JSON_SEED`
    /// and copied into the tmpfs $HOME before the agent runs. Never bound
    /// at its real path: many claude CLIs write it concurrently (rename
    /// over a lockfile), and two sandboxes sharing that bind race bwrap's
    /// own bind-mount setup.
    claude_json_seed: PathBuf,
    /// Operator-configured toolchain paths, read-only.
    extra_ro: Vec<PathBuf>,
    /// Operator-configured package caches, read-write.
    extra_rw: Vec<PathBuf>,
}

/// Quote `s` as a single POSIX shell argument.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
    /// `extra_ro` and `extra_rw` are bound alongside `paths.ro`/`paths.rw`;
    /// the caller resolves them (the executable's own directory, the cache
    /// directory) so detection stays a pure read of its inputs.
    pub fn detect(
        agent_bin: &str,
        paths: &crate::config::SandboxPaths,
        extra_ro: Vec<PathBuf>,
        extra_rw: Vec<PathBuf>,
    ) -> Result<Option<Sandbox>> {
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
            if k.starts_with("FORGE2_CLAUDE_BIN_") || k.starts_with("FORGE2_CODEX_BIN") {
                bins.push(v);
            }
        }
        // The codex CLI as well, when it is installed: a second runner's
        // binary has to be reachable inside the tmpfs home the same way the
        // claude CLI's is (tasks 274 to 278 exited at launch without it).
        // Optional, so a host without codex still sandboxes claude.
        let codex = crate::agent::codex_bin();
        if !bins.contains(&codex) && resolve_binary(&codex).is_ok() {
            bins.push(codex);
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
        let claude_json_seed = home.join(".claude.json");
        // The claude config directory holds its credentials; ~/.codex holds
        // codex's login and its per-thread state. Both are bound with
        // --bind-try, so a host without one of them is unaffected.
        let write_paths = vec![config_dir, home.join(".codex")];
        Ok(Some(Sandbox {
            bwrap,
            home,
            agent_dirs: agent_dirs.into_iter().collect(),
            write_paths,
            claude_json_seed,
            extra_ro: paths.ro.iter().cloned().chain(extra_ro).collect(),
            extra_rw: paths.rw.iter().cloned().chain(extra_rw).collect(),
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
        cmd.args(["--ro-bind-try"])
            .arg(&self.claude_json_seed)
            .arg(CLAUDE_JSON_SEED);
        cmd.arg("--bind").arg(worktree).arg(worktree);
        for p in self.write_paths.iter().chain(&self.extra_rw) {
            cmd.arg("--bind-try").arg(p).arg(p);
        }
        cmd.arg("--chdir").arg(worktree).arg("--");
        // The claude CLI's own config file, not the credential-bearing
        // config directory: seed it into the tmpfs $HOME as a real, private
        // file before exec, so the CLI's rename-over-a-lockfile update
        // never races another sandbox's copy of the same host file. Absent
        // on the host, the `--ro-bind-try` above is a no-op and this `cp`
        // silently does nothing, which is fine: the agent just starts
        // without one.
        let dest = self.home.join(".claude.json");
        let script = format!(
            "cp -f {} {} 2>/dev/null; exec \"$@\"",
            shell_quote(CLAUDE_JSON_SEED),
            shell_quote(&dest.to_string_lossy())
        );
        cmd.args(["/bin/sh", "-c"]).arg(script).arg("sh");
        cmd.args(argv);
        cmd.env_clear();
        cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd.env("HOME", &self.home);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_binds_tmpfs_home_before_ro_dirs_before_the_worktree() {
        let sandbox = Sandbox {
            bwrap: PathBuf::from("/usr/bin/bwrap"),
            home: PathBuf::from("/home/attempt"),
            agent_dirs: vec![PathBuf::from("/opt/agent")],
            write_paths: vec![PathBuf::from("/home/attempt/.claude")],
            claude_json_seed: PathBuf::from("/home/real/.claude.json"),
            extra_ro: vec![PathBuf::from("/opt/toolchain")],
            extra_rw: vec![PathBuf::from("/opt/cache")],
        };
        let worktree = PathBuf::from("/work/tree");
        let cmd = sandbox.command(&worktree, &["true".to_string()], &[]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let pos = |flag: &str, value: &str| {
            args.windows(2)
                .position(|w| w[0] == flag && w[1] == value)
                .unwrap_or_else(|| panic!("missing `{flag} {value}` in {args:?}"))
        };

        let tmpfs_home = pos("--tmpfs", "/home/attempt");
        let ro_agent = pos("--ro-bind-try", "/opt/agent");
        let ro_extra = pos("--ro-bind-try", "/opt/toolchain");
        let ro_seed = pos("--ro-bind-try", "/home/real/.claude.json");
        let worktree_bind = pos("--bind", "/work/tree");
        let rw_write = pos("--bind-try", "/home/attempt/.claude");
        let rw_extra = pos("--bind-try", "/opt/cache");

        assert!(tmpfs_home < ro_agent, "tmpfs $HOME must precede ro binds");
        assert!(tmpfs_home < ro_extra, "tmpfs $HOME must precede ro binds");
        assert!(
            ro_agent < worktree_bind,
            "agent ro bind must precede the worktree bind"
        );
        assert!(
            ro_extra < worktree_bind,
            "extra ro binds must precede the worktree bind"
        );
        assert!(
            ro_seed < worktree_bind,
            "the claude.json seed ro bind must precede the worktree bind"
        );
        assert!(
            worktree_bind < rw_write,
            "worktree bind must precede rw binds"
        );
        assert!(
            worktree_bind < rw_extra,
            "worktree bind must precede rw binds"
        );

        // The seed is bound read-only at a neutral path, never at the real
        // `.claude.json` path: nothing binds that path read-write anymore.
        assert!(
            !args.iter().any(|a| a == "/home/attempt/.claude.json"),
            "the sandbox must never bind the host file at the real .claude.json path: {args:?}"
        );
        let seed_dest_pos = args
            .windows(2)
            .position(|w| w[0] == "/home/real/.claude.json")
            .map(|i| i + 1)
            .expect("seed source arg present");
        assert_eq!(
            args[seed_dest_pos], "/run/forge/seed/claude.json",
            "seed must land at the neutral in-sandbox path"
        );

        // The sandboxed command is a copy-then-exec wrapper around the real
        // argv, not the real argv directly: `true` must not appear as argv[0].
        let dash_dash = args
            .iter()
            .position(|a| a == "--")
            .expect("-- separates bwrap flags from the sandboxed command");
        let tail = &args[dash_dash + 1..];
        assert_eq!(tail[0], "/bin/sh");
        assert_eq!(tail[1], "-c");
        assert!(
            tail[2].contains("/run/forge/seed/claude.json"),
            "wrapper must copy from the seed path: {}",
            tail[2]
        );
        assert!(
            tail[2].contains("/home/attempt/.claude.json"),
            "wrapper must copy to $HOME/.claude.json: {}",
            tail[2]
        );
        assert!(
            tail[2].contains("exec \"$@\""),
            "wrapper must exec the real argv after copying: {}",
            tail[2]
        );
        assert_eq!(tail[3], "sh", "argv[0] for the wrapper script is $0");
        assert_eq!(&tail[4..], &["true"], "the real argv follows the wrapper");
    }
}
