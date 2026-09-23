//! The agent and the checks run under bubblewrap. Read-only system, private
//! /tmp, /run and /proc, a tmpfs $HOME with only the holes the attempt
//! needs: the task's clone (its .git included), the agent binary, and a
//! private copy of the claude and codex CLIs' credentials and settings,
//! seeded from the operator's real state and discarded with the worktree
//! (see `provider_state_dir`, `discard_provider_state`) — the operator's
//! real `.claude`/`.codex` directories are never bound into a sandbox.
//! Nothing else on the host is visible, and in particular not the
//! registered checkout or its .git.
//!
//! The network is not shared. The sandbox has a network namespace of its
//! own with nothing in it but loopback; the only way out is the egress
//! proxy's unix socket, bound in and reached through a relay on
//! 127.0.0.1:3128 that HTTP_PROXY and HTTPS_PROXY name (see `egress`). The
//! proxy allows the model endpoint, always, and what the repository's
//! forge.toml declares under `[sandbox] egress`, and refuses the rest.
//!
//! Sandboxing is on by default and refuses to run without bwrap unless
//! `FORGE_SANDBOX=0` is set explicitly.

use crate::egress::{self, Policy, Proxies, Rule};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// Where the host's `~/.claude.json` is bound read-only inside the sandbox.
/// Not under `/opt`: on a host with a real `/opt`, that directory is itself
/// bound read-only a few flags earlier, and bwrap cannot create the
/// `forge/seed` path inside a read-only mount to bind onto. `/run` is a
/// tmpfs bwrap creates itself, so it always has room.
const CLAUDE_JSON_SEED: &str = "/run/forge/seed/claude.json";

/// Created by the egress relay once it is listening, in the sandbox's own
/// tmpfs `/run`.
const RELAY_READY: &str = "/run/forge/egress.ready";

pub struct Sandbox {
    bwrap: PathBuf,
    home: PathBuf,
    /// Directories holding the agent binary (as named and as resolved).
    agent_dirs: Vec<PathBuf>,
    /// The claude CLI's real config directory (credentials, settings) and
    /// codex's real `~/.codex`. Read from, on the host, only to seed each
    /// attempt's own private copy (see `command`); never bound into a
    /// sandbox themselves, so an attempt can neither read nor overwrite the
    /// operator's actual session state. These paths also happen to be
    /// where the sandbox's tmpfs `$HOME` puts the private copy, since
    /// `home` shadows the operator's real `$HOME` at the identical path.
    config_dir: PathBuf,
    codex_dir: PathBuf,
    /// The host's `~/.claude.json`, bound read-only at `CLAUDE_JSON_SEED`
    /// and copied into the tmpfs $HOME before the agent runs. Never bound
    /// at its real path: many claude CLIs write it concurrently (rename
    /// over a lockfile), and two sandboxes sharing that bind race bwrap's
    /// own bind-mount setup.
    claude_json_seed: PathBuf,
    /// Operator-configured toolchain paths, read-only.
    extra_ro: Vec<PathBuf>,
    /// Operator-configured package caches (`~/.npm`, `~/.cargo/registry`,
    /// ...): read from, an attempt's own writes going to a private overlay
    /// discarded with it (see `command`), so one attempt can never poison
    /// what another reads from the operator's real cache.
    extra_rw: Vec<PathBuf>,
    /// The model endpoints every attempt may reach, whatever its repository
    /// declares (see `egress::model_rules`).
    model_hosts: Vec<Rule>,
    /// The forge binary, bound in so the wrapper can run its egress relay.
    relay_exe: PathBuf,
    /// The proxies this process runs, one per distinct policy.
    proxies: Arc<Proxies>,
    /// A repository's declared egress, by the worktree its attempts run in
    /// (see `set_egress`); a worktree not in here gets the model endpoints
    /// alone.
    declared: Mutex<BTreeMap<PathBuf, Vec<Rule>>>,
    /// A repository's own cache directory (`FORGE_CACHE_DIR`), by the
    /// worktree its attempts run in (see `set_cache_dir`); a worktree not
    /// in here gets no cache bind at all. Keyed per repository so one
    /// repository's attempts can never poison a cache another reads.
    caches: Mutex<BTreeMap<PathBuf, PathBuf>>,
}

/// Quote `s` as a single POSIX shell argument.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Where the attempts in `worktree` get their private copy of the claude
/// and codex CLIs' state: a sibling of the worktree, under its parent, in
/// the same style as `attempt::tests_clone_dir`. Created and seeded by the
/// first `command` call, reseeded (credentials and settings only) by every
/// later one, and removed by `discard_provider_state` when the worktree
/// itself goes. It lives as long as the task, not one attempt: a capped or
/// failed attempt is resumed by `--resume <session>`, and the phase-two
/// report resumes the same thread, so the CLI's session transcripts must
/// survive between launches in one worktree. They never reach another
/// task's worktree, and the operator's real directories are never bound.
fn provider_state_dir(worktree: &Path) -> PathBuf {
    PathBuf::from(format!("{}-provider", worktree.display()))
}

/// Remove `worktree`'s private provider-state directory (see
/// `provider_state_dir`): called where the worktree itself is removed, so
/// nothing about the task's claude or codex sessions outlives its tree.
pub fn discard_provider_state(worktree: &Path) {
    let _ = std::fs::remove_dir_all(provider_state_dir(worktree));
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
    /// `Ok(None)` only when the operator opted out with FORGE_SANDBOX=0.
    /// `extra_ro` and `extra_rw` are bound alongside `paths.ro`/`paths.rw`;
    /// the caller resolves them (the executable's own directory) so
    /// detection stays a pure read of its inputs. A repository's own cache
    /// (`FORGE_CACHE_DIR`) is not here: it is declared per worktree, once
    /// known, with `set_cache_dir`.
    pub fn detect(
        agent_bin: &str,
        paths: &crate::config::SandboxPaths,
        extra_ro: Vec<PathBuf>,
        extra_rw: Vec<PathBuf>,
        model_hosts: Vec<Rule>,
    ) -> Result<Option<Sandbox>> {
        if crate::config::env("SANDBOX").as_deref() == Ok("0") {
            return Ok(None);
        }
        let Ok((bwrap, _)) = resolve_binary("bwrap") else {
            bail!("bwrap not found; install bubblewrap or set FORGE_SANDBOX=0 to run unsandboxed");
        };
        let home = PathBuf::from(std::env::var("HOME").context("HOME is not set")?);
        let mut agent_dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut bins = vec![agent_bin.to_string()];
        for (k, v) in std::env::vars() {
            if k.starts_with("FORGE_CLAUDE_BIN_")
                || k.starts_with("FORGE_CODEX_BIN")
                || k.starts_with("FORGE2_CLAUDE_BIN_")
                || k.starts_with("FORGE2_CODEX_BIN")
            {
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
        let codex_dir = home.join(".codex");
        let claude_json_seed = home.join(".claude.json");
        // The relay is this binary, so its directory has to be visible.
        let relay_exe = std::env::current_exe().context("finding the forge binary")?;
        let relay_dir = relay_exe.parent().map(Path::to_path_buf);
        Ok(Some(Sandbox {
            bwrap,
            home,
            agent_dirs: agent_dirs.into_iter().collect(),
            config_dir,
            codex_dir,
            claude_json_seed,
            extra_ro: paths
                .ro
                .iter()
                .cloned()
                .chain(extra_ro)
                .chain(relay_dir)
                .collect(),
            extra_rw: paths.rw.iter().cloned().chain(extra_rw).collect(),
            model_hosts,
            relay_exe,
            proxies: Arc::new(Proxies::default()),
            declared: Mutex::new(BTreeMap::new()),
            caches: Mutex::new(BTreeMap::new()),
        }))
    }

    /// Declare what attempts running in `worktree` may reach besides the
    /// model endpoints: the repository's `[sandbox] egress`, read from its
    /// trusted base. Called again whenever that config is re-read.
    pub fn set_egress(&self, worktree: &Path, rules: &[Rule]) {
        self.declared
            .lock()
            .unwrap()
            .insert(worktree.to_path_buf(), rules.to_vec());
    }

    /// Everything a command in `worktree` (or a directory below it) may
    /// reach: the model endpoints and what its repository declared.
    pub fn policy_for(&self, worktree: &Path) -> Policy {
        let declared = self.declared.lock().unwrap();
        let extra = worktree
            .ancestors()
            .find_map(|d| declared.get(d))
            .into_iter()
            .flatten();
        Policy::new(self.model_hosts.iter().chain(extra).cloned())
    }

    /// Declare where a command in `worktree` (or a directory below it) may
    /// cache what it computes: `dir`, private to the repository that owns
    /// `worktree`, so one repository's attempts can never read or poison
    /// what another cached (see `ctx::Forge::declare_cache`).
    pub fn set_cache_dir(&self, worktree: &Path, dir: PathBuf) {
        self.caches
            .lock()
            .unwrap()
            .insert(worktree.to_path_buf(), dir);
    }

    fn cache_dir_for(&self, worktree: &Path) -> Option<PathBuf> {
        let caches = self.caches.lock().unwrap();
        worktree.ancestors().find_map(|d| caches.get(d)).cloned()
    }

    /// Build the bwrap command that runs `argv` inside the worktree with
    /// exactly `env` (HOME is forced to the tmpfs home).
    pub fn command(&self, worktree: &Path, argv: &[String], env: &[(String, String)]) -> Command {
        let mut cmd = Command::new(&self.bwrap);
        cmd.args([
            "--die-with-parent",
            "--new-session",
            "--unshare-pid",
            "--unshare-net",
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
        // The route out: the proxy for this worktree's policy, on a socket
        // bound in beside the seed. Without a runtime to run a proxy on
        // there is no route, and the namespace has nothing but loopback.
        let socket = match self.proxies.socket_for(&self.policy_for(worktree)) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("egress: no route out for {}: {e:#}", worktree.display());
                None
            }
        };
        if let Some(s) = &socket {
            cmd.arg("--bind").arg(s).arg(egress::SANDBOX_SOCKET);
        }
        cmd.arg("--bind").arg(worktree).arg(worktree);
        // A private copy of the claude CLI's credentials and settings, and
        // of codex's login and config: seeded from the operator's real
        // files here (read, never bound into a sandbox themselves), then
        // bound writable at the paths each CLI expects. The directory is
        // the task's (see `provider_state_dir`): the seed files are
        // refreshed on every launch, everything else the CLIs wrote there
        // (session transcripts above all) is kept, so a resumed attempt and
        // the phase-two report find their thread. `discard_provider_state`
        // removes it with the worktree. The operator's real
        // `.claude`/`.codex` directories are never bound into a sandbox.
        let provider_dir = provider_state_dir(worktree);
        let claude_priv = provider_dir.join("claude");
        let codex_priv = provider_dir.join("codex");
        let _ = std::fs::create_dir_all(&claude_priv);
        let _ = std::fs::create_dir_all(&codex_priv);
        for name in [".credentials.json", "settings.json"] {
            let _ = std::fs::copy(self.config_dir.join(name), claude_priv.join(name));
        }
        for name in ["auth.json", "config.toml"] {
            let _ = std::fs::copy(self.codex_dir.join(name), codex_priv.join(name));
        }
        cmd.arg("--bind").arg(&claude_priv).arg(&self.config_dir);
        cmd.arg("--bind").arg(&codex_priv).arg(&self.codex_dir);
        // The operator's package caches: read through, an attempt's own
        // writes going to an invisible tmpfs overlay that bwrap discards
        // with the sandbox, so one attempt can never poison what another
        // reads from the operator's real cache. `--overlay-src` has no
        // `-try` form, so a cache the operator never populated is skipped
        // rather than failing the launch.
        for p in self.extra_rw.iter().filter(|p| p.exists()) {
            cmd.arg("--overlay-src").arg(p);
            cmd.arg("--tmp-overlay").arg(p);
        }
        // This repository's own cache (`FORGE_CACHE_DIR`), private to it
        // (see `ctx::Forge::declare_cache`): read-write, but never another
        // repository's, so one cannot poison a cache another reads.
        if let Some(dir) = self.cache_dir_for(worktree) {
            cmd.arg("--bind-try").arg(&dir).arg(&dir);
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
        // Then the egress relay, in the background: it dies with the
        // namespace when the command ends. Its ready file is what the
        // command waits for, so its first request never beats the listener.
        let relay = if socket.is_some() {
            format!(
                "{} egress-relay --ready {RELAY_READY} >/dev/null 2>&1 & \
                 i=0; while [ ! -e {RELAY_READY} ] && [ $i -lt 500 ]; do i=$((i+1)); sleep 0.01; done; ",
                shell_quote(&self.relay_exe.to_string_lossy())
            )
        } else {
            String::new()
        };
        let script = format!(
            "cp -f {} {} 2>/dev/null; {relay}exec \"$@\"",
            shell_quote(CLAUDE_JSON_SEED),
            shell_quote(&dest.to_string_lossy())
        );
        cmd.args(["/bin/sh", "-c"]).arg(script).arg("sh");
        cmd.args(argv);
        cmd.env_clear();
        cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd.env("HOME", &self.home);
        if socket.is_some() {
            let proxy = format!("http://{}", egress::RELAY_ADDR);
            for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
                cmd.env(k, &proxy);
            }
            // What the attempt runs on loopback (a test server) is not the
            // proxy's business.
            for k in ["NO_PROXY", "no_proxy"] {
                cmd.env(k, "localhost,127.0.0.1,::1");
            }
            // node only honours the proxy variables when asked to.
            cmd.env("NODE_USE_ENV_PROXY", "1");
        }
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_binds_tmpfs_home_before_ro_dirs_before_the_worktree() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join("work/tree");
        std::fs::create_dir_all(&worktree).unwrap();
        let config_dir = root.path().join("real/.claude");
        let codex_dir = root.path().join("real/.codex");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(config_dir.join(".credentials.json"), "creds").unwrap();
        std::fs::write(config_dir.join("settings.json"), "settings").unwrap();
        std::fs::write(codex_dir.join("auth.json"), "auth").unwrap();
        std::fs::write(codex_dir.join("config.toml"), "cfg").unwrap();
        let npm_cache = root.path().join("opt/npm-cache");
        std::fs::create_dir_all(&npm_cache).unwrap();
        let repo_cache = root.path().join("forge-home/cache/abc123");

        let sandbox = Sandbox {
            bwrap: PathBuf::from("/usr/bin/bwrap"),
            home: PathBuf::from("/home/attempt"),
            agent_dirs: vec![PathBuf::from("/opt/agent")],
            config_dir: config_dir.clone(),
            codex_dir: codex_dir.clone(),
            claude_json_seed: PathBuf::from("/home/real/.claude.json"),
            extra_ro: vec![PathBuf::from("/opt/toolchain")],
            extra_rw: vec![npm_cache.clone()],
            model_hosts: vec![Rule::parse("api.example.com").unwrap()],
            relay_exe: PathBuf::from("/opt/forge/forge"),
            proxies: Arc::new(Proxies::default()),
            declared: Mutex::new(BTreeMap::new()),
            caches: Mutex::new(BTreeMap::new()),
        };
        sandbox.set_cache_dir(&worktree, repo_cache.clone());
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
        let worktree_bind = pos("--bind", worktree.to_str().unwrap());
        let overlay_src = pos("--overlay-src", npm_cache.to_str().unwrap());
        let tmp_overlay = pos("--tmp-overlay", npm_cache.to_str().unwrap());
        let cache_bind = pos("--bind-try", repo_cache.to_str().unwrap());

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
            worktree_bind < overlay_src,
            "worktree bind must precede the package cache overlay"
        );
        assert!(
            worktree_bind < tmp_overlay,
            "worktree bind must precede the package cache overlay"
        );
        assert!(
            worktree_bind < cache_bind,
            "worktree bind must precede the repository cache bind"
        );

        // A private, seeded copy of the claude and codex state is bound
        // writable at the paths the CLIs expect; the operator's real
        // directories are never a bind source.
        let provider_dir = provider_state_dir(&worktree);
        let claude_priv = provider_dir.join("claude");
        let codex_priv = provider_dir.join("codex");
        let provider_bind = |src: &Path, dest: &Path| {
            args.windows(3)
                .position(|w| {
                    w[0] == "--bind"
                        && w[1] == src.to_str().unwrap()
                        && w[2] == dest.to_str().unwrap()
                })
                .unwrap_or_else(|| {
                    panic!("missing private provider bind {src:?} -> {dest:?}: {args:?}")
                })
        };
        let claude_bind = provider_bind(&claude_priv, &config_dir);
        let codex_bind = provider_bind(&codex_priv, &codex_dir);
        assert!(worktree_bind < claude_bind && worktree_bind < codex_bind);
        assert!(
            !args.windows(3).any(|w| matches!(
                w[0].as_str(),
                "--bind" | "--ro-bind" | "--bind-try" | "--ro-bind-try"
            ) && (w[1] == config_dir.to_str().unwrap()
                || w[1] == codex_dir.to_str().unwrap())),
            "the operator's real claude/codex directories must never be a bind source: {args:?}"
        );
        assert_eq!(
            std::fs::read_to_string(claude_priv.join(".credentials.json")).unwrap(),
            "creds"
        );
        assert_eq!(
            std::fs::read_to_string(claude_priv.join("settings.json")).unwrap(),
            "settings"
        );
        assert_eq!(
            std::fs::read_to_string(codex_priv.join("auth.json")).unwrap(),
            "auth"
        );
        assert_eq!(
            std::fs::read_to_string(codex_priv.join("config.toml")).unwrap(),
            "cfg"
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

        discard_provider_state(&worktree);
        assert!(!provider_dir.exists(), "provider state must be discarded");
    }

    fn test_sandbox(model: &str) -> Sandbox {
        Sandbox {
            bwrap: PathBuf::from("/usr/bin/bwrap"),
            home: PathBuf::from("/home/attempt"),
            agent_dirs: vec![],
            config_dir: PathBuf::from("/home/attempt/.claude"),
            codex_dir: PathBuf::from("/home/attempt/.codex"),
            claude_json_seed: PathBuf::from("/home/real/.claude.json"),
            extra_ro: vec![],
            extra_rw: vec![],
            model_hosts: vec![Rule::parse(model).unwrap()],
            relay_exe: PathBuf::from("/opt/forge/forge"),
            proxies: Arc::new(Proxies::default()),
            declared: Mutex::new(BTreeMap::new()),
            caches: Mutex::new(BTreeMap::new()),
        }
    }

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_worktrees_policy_is_the_model_endpoint_plus_what_its_repository_declared() {
        let sb = test_sandbox("api.example.com");
        let names = |p: &Policy| p.rules().iter().map(|r| r.to_string()).collect::<Vec<_>>();
        let wt = PathBuf::from("/work/1");
        assert_eq!(names(&sb.policy_for(&wt)), ["api.example.com"]);
        sb.set_egress(&wt, &[Rule::parse("registry.npmjs.org").unwrap()]);
        assert_eq!(
            names(&sb.policy_for(&wt)),
            ["api.example.com", "registry.npmjs.org"]
        );
        // A directory below the worktree shares its policy; a sibling does not.
        assert_eq!(names(&sb.policy_for(&wt.join("scratch"))).len(), 2);
        assert_eq!(
            names(&sb.policy_for(Path::new("/work/2"))),
            ["api.example.com"]
        );
    }

    #[tokio::test]
    async fn the_command_has_a_namespace_of_its_own_and_one_route_out() {
        let sb = test_sandbox("api.example.com");
        let cmd = sb.command(Path::new("/work/1"), &["true".to_string()], &[]);
        let args = args_of(&cmd);
        assert!(args.iter().any(|a| a == "--unshare-net"), "{args:?}");
        let bind = args
            .windows(3)
            .find(|w| w[0] == "--bind" && w[2] == "/run/forge/egress.sock")
            .unwrap_or_else(|| panic!("the proxy socket is bound in: {args:?}"));
        assert!(
            Path::new(&bind[1]).exists(),
            "the socket exists on the host"
        );
        let script = &args[args.iter().position(|a| a == "--").unwrap() + 3];
        assert!(
            script.contains("'/opt/forge/forge' egress-relay"),
            "{script}"
        );
        assert!(
            script.find("egress-relay").unwrap() < script.find("exec \"$@\"").unwrap(),
            "the relay starts before the command: {script}"
        );
        let env = |k: &str| {
            cmd.get_envs()
                .find(|(n, _)| *n == k)
                .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
        };
        for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            assert_eq!(env(k).as_deref(), Some("http://127.0.0.1:3128"), "{k}");
        }
        assert_eq!(env("NO_PROXY").as_deref(), Some("localhost,127.0.0.1,::1"));
    }

    #[test]
    fn without_a_runtime_there_is_no_route_but_the_network_is_still_unshared() {
        let sb = test_sandbox("api.example.com");
        let cmd = sb.command(Path::new("/work/1"), &["true".to_string()], &[]);
        let args = args_of(&cmd);
        assert!(args.iter().any(|a| a == "--unshare-net"), "{args:?}");
        assert!(
            !args.iter().any(|a| a == "/run/forge/egress.sock"),
            "{args:?}"
        );
        assert!(
            cmd.get_envs().all(|(k, _)| k != "HTTPS_PROXY"),
            "no proxy is named when there is none"
        );
    }
}
