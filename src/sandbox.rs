//! The agent and the checks run under bubblewrap. Read-only system, private
//! /tmp, /run and /proc, a tmpfs $HOME with only the holes the attempt
//! needs: the task's clone (its .git included), the agent binary, and a
//! private copy of the CLIs' credentials with kernel-built settings,
//! seeded from the operator's logins and discarded with the worktree
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
//! Sandboxing is on by default where bwrap is installed. A machine without
//! it (macOS) runs repositories that declare no `[execution] backend` on
//! the host (`executor::default_backend`), and `forge doctor` warns what
//! that forgoes; one that declares `backend = "bwrap"` still refuses.

use crate::config::SandboxLimits as ResourceLimits;
use crate::egress::{self, Policy, Proxies, Rule};
use crate::workflows::Contract;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// The only user config Claude needs in the private home.
const CLAUDE_JSON_SEED: &str = r#"{"hasCompletedOnboarding":true,"theme":"dark"}"#;

/// Created by the egress relay once it is listening, in the sandbox's own
/// tmpfs `/run`.
const RELAY_READY: &str = "/run/forge/egress.ready";
const RELAY_LOG: &str = "/run/forge/egress-relay.log";
pub(crate) const RELAY_START_FAILED: &str = "forge: the egress relay did not start";

/// Repository commands must not inherit the agent's private provider state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Agent,
    Check,
}

impl ResourceLimits {
    fn scope_args(self, cmd: &mut Command) {
        // A scope launcher (including the availability probe) must not
        // report its status to the worker's Type=notify service socket.
        cmd.env_remove("NOTIFY_SOCKET");
        cmd.args(["--user", "--scope", "--quiet"])
            .arg(format!("--property=MemoryMax={}", self.memory_max))
            .arg(format!("--property=TasksMax={}", self.tasks_max))
            .arg("--");
    }

    /// Check the manager and controller delegation, not just the executable.
    pub fn scope_runner(self) -> Option<PathBuf> {
        let (runner, _) = resolve_binary("systemd-run").ok()?;
        let mut cmd = Command::new(&runner);
        self.scope_args(&mut cmd);
        let mut child = cmd
            .arg("/bin/true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success().then_some(runner),
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
    }
}

pub struct Sandbox {
    limits: ResourceLimits,
    scope_runner: Option<PathBuf>,
    bwrap: PathBuf,
    home: PathBuf,
    /// Directories holding the agent binary (as named and as resolved).
    agent_dirs: Vec<PathBuf>,
    /// The claude CLI's real config directory (credentials, settings) and
    /// codex's real `~/.codex`. Read from, on the host, only to seed each
    /// attempt's own private copy (see `command`); never bound into a
    /// sandbox themselves, so an attempt cannot read the operator's actual
    /// session state, and cannot overwrite it either: the kernel writes a
    /// private login back over the real one only when the CLI could have
    /// produced it from its seed (see `login`). These paths also happen to
    /// be where the sandbox's tmpfs `$HOME` puts the private copy, since
    /// `home` shadows the operator's real `$HOME` at the identical path.
    config_dir: PathBuf,
    /// FORGE_HOME: where the kernel records what it seeded into each
    /// private login, out of any sandbox's reach (see `login`).
    forge_home: PathBuf,
    codex_dir: PathBuf,
    /// copilot's real `~/.copilot`: read from, on the host, only to seed
    /// each sandbox's private copy of its `config.json` (the login), never
    /// bound in itself.
    copilot_dir: PathBuf,
    /// Operator-configured toolchain paths, read-only.
    extra_ro: Vec<PathBuf>,
    /// Operator-configured package caches (`~/.npm`, `~/.cargo/registry`,
    /// ...): read from, an attempt's own writes going to a private overlay
    /// discarded with it (see `command`), so one attempt can never poison
    /// what another reads from the operator's real cache.
    extra_rw: Vec<PathBuf>,
    /// Whether the bwrap found has `--overlay-src`/`--overlay` (0.10.0
    /// and later), probed once in `detect`. Without it the package caches
    /// are not bound at all.
    overlay: bool,
    /// The operator-warmed dependency cache, read-only (`[sandbox]
    /// dependency_cache`).
    dependency_cache: Option<PathBuf>,
    /// The forge binary, bound in so the wrapper can run its egress relay.
    relay_exe: PathBuf,
    /// Whether a launch starts the egress relay when a route exists. Off
    /// only for the test constructor, whose relay binary does not exist.
    relay: bool,
    /// The proxies this process runs, one per distinct policy.
    proxies: Arc<Proxies>,
    /// A repository's declared egress, by the worktree its attempts run in
    /// (see `set_egress`); a worktree not in here gets the model endpoints
    /// alone.
    declared: Mutex<BTreeMap<PathBuf, Vec<Rule>>>,
    /// The model endpoints of the provider each worktree's step resolved
    /// (see `set_provider_hosts`), by the worktree its attempts run in; a
    /// worktree not in here gets no model endpoint at all (nothing has
    /// resolved a provider for it yet).
    provider_hosts: Mutex<BTreeMap<PathBuf, Vec<Rule>>>,
    /// A repository's own cache directory (`FORGE_CACHE_DIR`), by the
    /// worktree its attempts run in (see `set_cache_dir`); a worktree not
    /// in here gets no cache bind at all. Keyed per repository so one
    /// repository's attempts can never poison a cache another reads.
    caches: Mutex<BTreeMap<PathBuf, PathBuf>>,
    targets: Mutex<BTreeMap<PathBuf, PathBuf>>,
    /// What the environment policy granted a worktree's attempts after a
    /// failure (see `environment`): hosts on top of the declared egress,
    /// host cache paths bound read-only. Kept apart from `declared` so
    /// re-reading the repository's config never drops a grant.
    granted: Mutex<BTreeMap<PathBuf, Granted>>,
}

/// One worktree's environment grants.
#[derive(Default)]
struct Granted {
    hosts: Vec<Rule>,
    ro: Vec<PathBuf>,
}

/// The first bwrap with `--overlay-src` and `--overlay`.
pub const OVERLAY_MIN: (u64, u64, u64) = (0, 10, 0);

/// A bwrap version: `major.minor.patch` plus an optional prerelease tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl std::fmt::Display for BwrapVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

/// Parse the version out of `bwrap --version` output (`bubblewrap 0.9.0`).
pub fn parse_bwrap_version(out: &str) -> Option<BwrapVersion> {
    let word = out
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    let (core, pre) = match word.split_once('-') {
        Some((core, pre)) => (core, Some(pre.to_string()).filter(|p| !p.is_empty())),
        None => (word, None),
    };
    let mut it = core.split('.').map(|n| {
        let digits: String = n.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u64>().ok()
    });
    let major = it.next()??;
    let minor = it.next().flatten().unwrap_or(0);
    let patch = it.next().flatten().unwrap_or(0);
    Some(BwrapVersion {
        major,
        minor,
        patch,
        pre,
    })
}

/// Run `bwrap --version` and parse it; `None` when it cannot be run or read.
pub fn bwrap_version(bwrap: &Path) -> Option<BwrapVersion> {
    for attempt in 0..20 {
        match Command::new(bwrap).arg("--version").output() {
            // Freshly installed executables can remain briefly busy after
            // their writer closes, just like freshly written agent scripts.
            Err(err) if err.raw_os_error() == Some(libc::ETXTBSY) && attempt < 19 => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => {
                let out = result.ok()?;
                return parse_bwrap_version(&String::from_utf8_lossy(&out.stdout));
            }
        }
    }
    unreachable!()
}

/// Whether a bwrap of this version has overlay support. An unreadable
/// version counts as no support: the degraded launch always works. A
/// prerelease of the minimum sorts below it, the semver way.
pub fn version_has_overlay(v: Option<BwrapVersion>) -> bool {
    v.is_some_and(|v| {
        let core = (v.major, v.minor, v.patch);
        core > OVERLAY_MIN || (core == OVERLAY_MIN && v.pre.is_none())
    })
}

/// Quote `s` as a single POSIX shell argument.
pub(crate) fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Kernel-owned disk uppers, never bound directly into an attempt.
fn overlay_state_dir(worktree: &Path) -> PathBuf {
    PathBuf::from(format!("{}-overlays", worktree.display()))
}

/// Where the attempts in `worktree` get their private copy of the claude
/// and codex CLIs' state: a sibling of the worktree, under its parent, in
/// the same style as `attempt::tests_clone_dir`. Created and seeded by the
/// first launch's `Sandbox::prepare`, reseeded (credentials
/// only) by every later one, and removed by `discard_provider_state` when the worktree
/// itself goes. It lives as long as the task, not one attempt: a capped or
/// failed attempt is resumed by `--resume <session>`, and the phase-two
/// report resumes the same thread, so the CLI's session transcripts must
/// survive between launches in one worktree. They never reach another
/// task's worktree, and the operator's real directories are never bound.
fn provider_state_dir(worktree: &Path) -> PathBuf {
    PathBuf::from(format!("{}-provider", worktree.display()))
}

/// Keep review transcripts separate from the task's writing steps.
fn provider_dir_for(worktree: &Path, contract: Option<Contract>) -> PathBuf {
    if contract == Some(Contract::Review) {
        PathBuf::from(format!("{}-review-provider", worktree.display()))
    } else {
        provider_state_dir(worktree)
    }
}

/// The contract a launch's environment names, which picks its provider
/// directory.
fn contract_of(env: &[(String, String)]) -> Option<Contract> {
    env.iter()
        .rev()
        .find(|(k, _)| k == "FORGE_CONTRACT")
        .and_then(|(_, v)| Contract::parse(v))
}

/// Remove `worktree`'s private provider-state directory (see
/// `provider_state_dir`): called where the worktree itself is removed, so
/// nothing about the task's claude or codex sessions outlives its tree. A
/// copy still holding a login later than the host's is written back first
/// (docs/REVIEW-4.md #1.9): deleting it unwritten could throw away the only
/// live refresh token. The directory is kept, not removed, when that
/// write-back fails, so a later launch gets another chance at it.
pub fn discard_provider_state(worktree: &Path) {
    let state = crate::ctx::Paths::compute_home().ok();
    discard_provider_state_in(worktree, state.as_deref(), |shape| shape.config_dir());
}

/// `discard_provider_state`, with `state` (FORGE_HOME) and each shape's host
/// directory taken as arguments rather than read from the ambient
/// environment: what makes the write-before-removal guard testable without
/// a process-wide `std::env::set_var` (see `git::tests::identity_falls_back…`
/// for why that is avoided here).
fn discard_provider_state_in(
    worktree: &Path,
    state: Option<&Path>,
    host_dir: impl Fn(&crate::login::Shape) -> Option<PathBuf>,
) {
    let _ = std::fs::remove_dir_all(overlay_state_dir(worktree));
    for contract in [None, Some(Contract::Review)] {
        let dir = provider_dir_for(worktree, contract);
        if let Some(state) = state {
            let mut failed = false;
            for shape in crate::login::SHAPES {
                let private = dir.join(shape.cli).join(shape.file);
                if !crate::login::is_regular_file(&private) {
                    continue;
                }
                let Some(host) = host_dir(shape) else {
                    continue;
                };
                if shape.write_back_sync(&host, state, &private).failed() {
                    failed = true;
                }
            }
            if failed {
                continue;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
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

/// The claude, codex and copilot CLIs' state directories, each named by its
/// own environment variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
/// `COPILOT_HOME`, all passed into the sandbox by `agent_env`) or else
/// under `home`. An empty value counts as unset.
fn provider_dirs(
    home: &Path,
    var: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> (PathBuf, PathBuf, PathBuf) {
    let dir = |name: &str, default: &str| {
        var(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(default))
    };
    (
        dir("CLAUDE_CONFIG_DIR", ".claude"),
        dir("CODEX_HOME", ".codex"),
        dir("COPILOT_HOME", ".copilot"),
    )
}

/// A read-only entry that is itself a private provider directory would be
/// bound and then bound over, and the operator's meaning lost: refuse it,
/// naming the path.
fn refuse_provider_dir_binds<'a>(
    entries: impl Iterator<Item = &'a PathBuf>,
    provider: [&PathBuf; 3],
) -> Result<()> {
    for d in entries {
        if provider.contains(&d) {
            bail!(
                "{} is bound read-only into the sandbox but is also a CLI's private state directory; bind something inside it instead, or move it",
                d.display()
            );
        }
    }
    Ok(())
}

impl Sandbox {
    /// `extra_ro` and `extra_rw` are bound alongside `paths.ro`/`paths.rw`;
    /// the caller resolves them (the executable's own directory) so
    /// detection stays a pure read of its inputs. A repository's own cache
    /// (`FORGE_CACHE_DIR`) is not here: it is declared per worktree, once
    /// known, with `set_cache_dir`.
    pub fn detect(
        agent_bin: &str,
        paths: &crate::config::SandboxPaths,
        forge_home: PathBuf,
        extra_ro: Vec<PathBuf>,
        extra_rw: Vec<PathBuf>,
    ) -> Result<Sandbox> {
        let Ok((bwrap, _)) = resolve_binary("bwrap") else {
            bail!(
                "bwrap not found; install bubblewrap, or declare [execution] backend = \"host\" to run unsandboxed"
            );
        };
        let overlay = version_has_overlay(bwrap_version(&bwrap));
        let home = PathBuf::from(std::env::var("HOME").context("HOME is not set")?);
        let mut agent_dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut bins = vec![agent_bin.to_string()];
        for (k, v) in std::env::vars() {
            if k.starts_with("FORGE_CLAUDE_BIN_")
                || k.starts_with("FORGE_CODEX_BIN")
                || k.starts_with("FORGE_COPILOT_BIN")
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
        // And the copilot CLI, on the same terms.
        let copilot = crate::agent::copilot_bin();
        if !bins.contains(&copilot) && resolve_binary(&copilot).is_ok() {
            bins.push(copilot);
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
        let (config_dir, codex_dir, copilot_dir) = provider_dirs(&home, |k| std::env::var_os(k));
        // The relay is this binary, so its directory has to be visible.
        // Resolved to the real file: the named path is often a symlink
        // chain (~/.local/bin/forge -> bin/current -> releases/<id>) whose
        // intermediate directories are not bound, so inside the sandbox
        // only the target's own directory is guaranteed to exist.
        let named = crate::binary::without_deleted_suffix(&crate::binary::launch_path()?);
        let relay_exe = std::fs::canonicalize(&named).unwrap_or(named);
        let relay_dir = relay_exe.parent().map(Path::to_path_buf);
        let extra_ro: Vec<PathBuf> = paths
            .ro
            .iter()
            .cloned()
            .chain(extra_ro)
            .chain(relay_dir)
            .collect();
        let agent_dirs: Vec<PathBuf> = agent_dirs.into_iter().collect();
        refuse_provider_dir_binds(
            agent_dirs.iter().chain(&extra_ro),
            [&config_dir, &codex_dir, &copilot_dir],
        )?;
        Ok(Sandbox {
            limits: paths.limits,
            scope_runner: paths.limits.scope_runner(),
            bwrap,
            home,
            agent_dirs,
            config_dir,
            forge_home,
            codex_dir,
            copilot_dir,
            extra_ro,
            extra_rw: paths.rw.iter().cloned().chain(extra_rw).collect(),
            overlay,
            dependency_cache: paths.dependency_cache.clone(),
            relay_exe,
            relay: true,
            proxies: Arc::new(Proxies::default()),
            declared: Mutex::new(BTreeMap::new()),
            provider_hosts: Mutex::new(BTreeMap::new()),
            caches: Mutex::new(BTreeMap::new()),
            targets: Mutex::new(BTreeMap::new()),
            granted: Mutex::new(BTreeMap::new()),
        })
    }

    /// The read-only entries, split into those outside every private
    /// provider directory and those inside one. The second kind must be bound
    /// after the private binds, or the private directory shadows them.
    fn split_under_provider_dirs(&self) -> (Vec<&PathBuf>, Vec<&PathBuf>) {
        let dirs = [&self.config_dir, &self.codex_dir, &self.copilot_dir];
        self.agent_dirs
            .iter()
            .chain(&self.extra_ro)
            .partition(|d| dirs.iter().any(|p| d.starts_with(p) && d != p))
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

    /// Declare the model endpoints attempts running in `worktree` may
    /// reach: the provider its step resolved (see `egress::provider_rules`),
    /// never every configured provider's. Called beside `set_egress`
    /// wherever `ctx::Forge::allow_egress` is.
    pub fn set_provider_hosts(&self, worktree: &Path, rules: &[Rule]) {
        self.provider_hosts
            .lock()
            .unwrap()
            .insert(worktree.to_path_buf(), rules.to_vec());
    }

    /// Grant attempts in `worktree` one more host. `false` when it was
    /// already granted, so a caller re-running on a grant cannot loop.
    pub fn grant_host(&self, worktree: &Path, rule: Rule) -> bool {
        let mut g = self.granted.lock().unwrap();
        let hosts = &mut g.entry(worktree.to_path_buf()).or_default().hosts;
        if hosts.iter().any(|r| r.to_string() == rule.to_string()) {
            return false;
        }
        hosts.push(rule);
        true
    }

    /// Bind `path` read-only into attempts in `worktree`. `false` when it
    /// was already granted.
    pub fn grant_ro(&self, worktree: &Path, path: PathBuf) -> bool {
        let mut g = self.granted.lock().unwrap();
        let ro = &mut g.entry(worktree.to_path_buf()).or_default().ro;
        if ro.contains(&path) {
            return false;
        }
        ro.push(path);
        true
    }

    /// Everything a command in `worktree` (or a directory below it) may
    /// reach: the model endpoints of the provider its step resolved, what
    /// its repository declared and what the environment policy granted it.
    pub fn policy_for(&self, worktree: &Path) -> Policy {
        let provider_hosts = self.provider_hosts.lock().unwrap();
        let declared = self.declared.lock().unwrap();
        let granted = self.granted.lock().unwrap();
        let model = worktree
            .ancestors()
            .find_map(|d| provider_hosts.get(d))
            .into_iter()
            .flatten();
        let extra = worktree
            .ancestors()
            .find_map(|d| declared.get(d))
            .into_iter()
            .flatten();
        let more = worktree
            .ancestors()
            .find_map(|d| granted.get(d))
            .into_iter()
            .flat_map(|g| g.hosts.iter());
        Policy::new(model.chain(extra).chain(more).cloned())
    }

    /// Declare where a command in `worktree` (or a directory below it) may
    /// cache what it computes: `dir`, private to the repository that owns
    /// `worktree`, so one repository's attempts can never read or poison
    /// what another cached (see `ctx::Forge::declare_cache`).
    pub fn set_target_dir(&self, worktree: &Path, target: PathBuf) {
        self.targets
            .lock()
            .unwrap()
            .insert(worktree.to_path_buf(), target);
    }

    pub fn set_cache_dir(&self, worktree: &Path, dir: PathBuf) {
        self.caches
            .lock()
            .unwrap()
            .insert(worktree.to_path_buf(), dir);
    }

    /// The real config directory of `shape`'s CLI, which its login is
    /// seeded from and written back to.
    fn login_dir(&self, shape: &crate::login::Shape) -> &Path {
        match shape.cli {
            "codex" => &self.codex_dir,
            "copilot" => &self.copilot_dir,
            _ => &self.config_dir,
        }
    }

    /// Copy `worktree`'s private `shape` login back over the host file when
    /// the attempt refreshed it (see `login`). Whether it did.
    pub async fn write_back_login(&self, shape: &crate::login::Shape, worktree: &Path) -> bool {
        let mut any = false;
        for contract in [None, Some(Contract::Review)] {
            let private = provider_dir_for(worktree, contract)
                .join(shape.cli)
                .join(shape.file);
            any |= shape
                .write_back(self.login_dir(shape), &self.forge_home, &private)
                .await
                .wrote();
        }
        any
    }

    /// `write_back_login` of the claude login for every task's private copy
    /// beside `worktree`, for a caller that holds the login's lock (see
    /// `login::lock`).
    pub fn write_back_siblings_locked(&self, worktree: &Path) {
        crate::login::CLAUDE.write_back_private_copies_locked(
            &self.config_dir,
            &self.forge_home,
            worktree,
        );
    }

    fn cache_dir_for(&self, worktree: &Path) -> Option<PathBuf> {
        let caches = self.caches.lock().unwrap();
        worktree.ancestors().find_map(|d| caches.get(d)).cloned()
    }

    /// A sandbox whose bwrap is `bwrap` (a fake, in a test) and whose every
    /// host path lives under `home`.
    #[cfg(test)]
    pub(crate) fn with_bwrap(bwrap: PathBuf, home: PathBuf) -> Sandbox {
        Sandbox {
            limits: ResourceLimits::default(),
            scope_runner: None,
            bwrap,
            config_dir: home.join(".claude"),
            forge_home: home.join("forge-home"),
            codex_dir: home.join(".codex"),
            copilot_dir: home.join(".copilot"),
            home,
            agent_dirs: vec![],
            extra_ro: vec![],
            extra_rw: vec![],
            overlay: true,
            dependency_cache: None,
            relay_exe: PathBuf::from("/nonexistent/forge"),
            relay: false,
            proxies: Arc::new(Proxies::default()),
            declared: Mutex::new(BTreeMap::new()),
            provider_hosts: Mutex::new(BTreeMap::new()),
            caches: Mutex::new(BTreeMap::new()),
            targets: Mutex::new(BTreeMap::new()),
            granted: Mutex::new(BTreeMap::new()),
        }
    }

    /// Build the bwrap command that runs `argv` inside the worktree with
    /// exactly `env` (HOME is forced to the tmpfs home).
    #[cfg(test)]
    pub fn command_for_worktree(
        &self,
        worktree: &Path,
        argv: &[String],
        env: &[(String, String)],
    ) -> Command {
        self.command(
            worktree,
            argv,
            env,
            &self.policy_for(worktree),
            Phase::Agent,
        )
    }

    /// Whether the proxy socket this worktree's launch will bind in is
    /// there: an error naming it when not, so the launch is an environment
    /// fault rather than a `bwrap` failure the agent is blamed for.
    pub fn check_socket(&self, worktree: &Path) -> Result<()> {
        // No runtime means no route, which `command` already tolerates.
        let Ok(socket) = self.proxies.socket_for(&self.policy_for(worktree)) else {
            return Ok(());
        };
        anyhow::ensure!(
            socket.exists(),
            "egress proxy socket {} is missing",
            socket.display()
        );
        Ok(())
    }

    fn wrapper_script(&self, relay_enabled: bool, refused: Option<&Path>, phase: Phase) -> String {
        // Build the agent config in the tmpfs home without reading operator state.
        let dest = self.home.join(".claude.json");
        // Then the egress relay, in the background: it dies with the
        // namespace when the command ends. Its ready file is what the
        // command waits for, so its first request never beats the listener.
        // Refusals are recorded where the attempt that ran into them reads
        // them back (`egress::refused_path`).
        let record = refused.map_or(String::new(), |p| {
            format!(" --refused {}", shell_quote(&p.to_string_lossy()))
        });
        let relay = if relay_enabled {
            format!(
                "{} egress-relay --ready {RELAY_READY}{record} >/dev/null 2>{RELAY_LOG} & \
                 i=0; while [ ! -e {RELAY_READY} ] && [ $i -lt 500 ]; do i=$((i+1)); sleep 0.01; done; \
                 if [ ! -e {RELAY_READY} ]; then \
                 echo '{RELAY_START_FAILED}' >&2; cat {RELAY_LOG} >&2; exit 125; fi; ",
                shell_quote(&self.relay_exe.to_string_lossy())
            )
        } else {
            String::new()
        };
        let seed = if phase == Phase::Agent {
            format!(
                "printf '%s\\n' {} > {} || exit; ",
                shell_quote(CLAUDE_JSON_SEED),
                shell_quote(&dest.to_string_lossy())
            )
        } else {
            String::new()
        };
        // POSIX sh expresses RLIMIT_FSIZE in 512-byte blocks. With neither
        // -S nor -H, these lower both limits; the attempt cannot raise them.
        format!(
            "ulimit -c 0 && ulimit -f {} && ulimit -n 4096 || exit; {seed}{relay}exec \"$@\"",
            self.limits.memory_max / 512
        )
    }

    /// Everything a launch in `worktree` with `env` does to the host before
    /// its `command` is built: seed the step's private logins
    /// from the operator's (see `login`), waiting on the logins' locks
    /// without holding a thread (docs/REVIEW-4.md #1.10). Every launch
    /// awaits this first; checks skip seeding entirely. `command` itself
    /// never touches a login.
    pub async fn prepare(&self, worktree: &Path, env: &[(String, String)], phase: Phase) {
        if phase == Phase::Check {
            return;
        }
        let provider_dir = provider_dir_for(worktree, contract_of(env));
        for cli in ["claude", "codex", "copilot"] {
            let _ = std::fs::create_dir_all(provider_dir.join(cli));
        }
        // Each login is the kernel's (see `login`): a later private login is
        // written back over the host file first, and an empty host file
        // seeds nothing. copilot's login lives in its `config.json`, beside
        // its settings; only login fields are seeded.
        for shape in crate::login::SHAPES {
            shape
                .seed(
                    self.login_dir(shape),
                    &self.forge_home,
                    worktree,
                    &provider_dir.join(shape.cli).join(shape.file),
                )
                .await;
        }
        // Remove settings left by older kernels; never read the host copy.
        let _ = std::fs::remove_file(provider_dir.join("claude/settings.json"));
        let config = env
            .iter()
            .rev()
            .find(|(k, _)| k == "FORGE_CODEX_CONFIG")
            .map_or("", |(_, v)| v.as_str());
        let _ = crate::login::replace_atomic(
            &provider_dir.join("codex/config.toml"),
            config.as_bytes(),
        );
    }

    fn bind_provider_state(
        &self,
        cmd: &mut Command,
        worktree: &Path,
        env: &[(String, String)],
        phase: Phase,
    ) {
        // Private logins and kernel-built settings are bound writable at
        // the paths each CLI expects. Only logins are read from the host.
        // The directory is
        // the step's (see `provider_dir_for`): the seed files are
        // refreshed on every launch, everything else the CLIs wrote there
        // (session transcripts above all) is kept, so a resumed attempt and
        // the phase-two report find their thread. `discard_provider_state`
        // removes it with the worktree. The operator's real
        // `.claude`/`.codex` directories are never bound into a sandbox.
        if phase == Phase::Agent {
            let provider_dir = provider_dir_for(worktree, contract_of(env));
            let claude_priv = provider_dir.join("claude");
            let codex_priv = provider_dir.join("codex");
            let copilot_priv = provider_dir.join("copilot");
            for d in [&claude_priv, &codex_priv, &copilot_priv] {
                let _ = std::fs::create_dir_all(d);
            }
            cmd.arg("--bind").arg(&claude_priv).arg(&self.config_dir);
            cmd.arg("--bind").arg(&codex_priv).arg(&self.codex_dir);
            cmd.arg("--bind").arg(&copilot_priv).arg(&self.copilot_dir);
        } else {
            // Fresh private mounts on every check: no login seeding, no
            // session state from an agent or a previous check to read or modify.
            // Cover both configured locations and the defaults used when
            // provider environment variables are absent.
            let dirs = BTreeSet::from([
                self.config_dir.clone(),
                self.codex_dir.clone(),
                self.copilot_dir.clone(),
                self.home.join(".claude"),
                self.home.join(".codex"),
                self.home.join(".copilot"),
            ]);
            for dir in dirs {
                cmd.arg("--tmpfs").arg(dir);
            }
        }
    }

    pub fn command(
        &self,
        worktree: &Path,
        argv: &[String],
        env: &[(String, String)],
        policy: &Policy,
        phase: Phase,
    ) -> Command {
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
        cmd.arg("--size")
            .arg(self.limits.tmp_bytes.to_string())
            .args(["--tmpfs", "/tmp"]);
        cmd.args(["--size", "67108864", "--tmpfs", "/run"]);
        // systemd-resolved keeps the real resolv.conf under /run.
        cmd.args([
            "--ro-bind-try",
            "/run/systemd/resolve",
            "/run/systemd/resolve",
        ]);
        cmd.args(["--size", "268435456", "--tmpfs"]).arg(&self.home);
        // Order matters: everything under $HOME is bound after its tmpfs, and
        // the writable worktree after the read-only agent directory in case
        // one contains the other.
        let (under, ro) = self.split_under_provider_dirs();
        for d in ro {
            cmd.arg("--ro-bind-try").arg(d).arg(d);
        }
        // The route out: the proxy for this worktree's policy, on a socket
        // bound in beside the seed. Without a runtime to run a proxy on
        // there is no route, and the namespace has nothing but loopback.
        let socket = match self.proxies.socket_for(policy) {
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
        self.bind_provider_state(&mut cmd, worktree, env, phase);
        // What lives inside a directory just bound over (an agent under
        // `~/.claude/local`) is bound again on top of the private copy.
        for d in under {
            cmd.arg("--ro-bind-try").arg(d).arg(d);
        }
        // Disk-backed uppers avoid bwrap's unbounded invisible tmpfs. Each
        // launch gets fresh directories, outside all writable sandbox binds;
        // they are discarded alongside the worktree. If disk setup fails,
        // use a cold cache, never the operator's writable cache.
        if self.overlay {
            let root = overlay_state_dir(worktree);
            for p in self.extra_rw.iter().filter(|p| p.exists()) {
                let upper = (|| -> std::io::Result<_> {
                    std::fs::create_dir_all(&root)?;
                    let dir = tempfile::tempdir_in(&root)?;
                    std::fs::create_dir(dir.path().join("upper"))?;
                    std::fs::create_dir(dir.path().join("work"))?;
                    Ok(dir.keep())
                })();
                if let Ok(dir) = upper {
                    cmd.arg("--overlay-src")
                        .arg(p)
                        .arg("--overlay")
                        .arg(dir.join("upper"))
                        .arg(dir.join("work"))
                        .arg(p);
                }
            }
        }
        if let Some(d) = &self.dependency_cache {
            cmd.arg("--ro-bind-try").arg(d).arg(d);
        }
        // Host caches the environment policy granted this worktree.
        let granted = self.granted.lock().unwrap();
        if let Some(g) = worktree.ancestors().find_map(|d| granted.get(d)) {
            for d in &g.ro {
                cmd.arg("--ro-bind-try").arg(d).arg(d);
            }
        }
        drop(granted);
        // This repository's own cache (`FORGE_CACHE_DIR`), private to it
        // (see `ctx::Forge::declare_cache`): read-write, but never another
        // repository's, so one cannot poison a cache another reads.
        if let Some(dir) = self.cache_dir_for(worktree) {
            cmd.arg("--bind-try").arg(&dir).arg(&dir);
        }
        if let Some(target) = self.targets.lock().unwrap().get(worktree) {
            cmd.arg("--bind").arg(target).arg(target);
        }
        // Mask the credential store last, even when a broad cache grant or
        // custom FORGE_HOME made its parent reachable through another bind.
        let secrets = self.forge_home.join("secrets");
        if secrets.exists() {
            cmd.arg("--tmpfs").arg(&secrets);
        }
        if self.scope_runner.is_some() {
            cmd.args([
                "--unsetenv",
                "DBUS_SESSION_BUS_ADDRESS",
                "--unsetenv",
                "XDG_RUNTIME_DIR",
            ]);
        }
        cmd.arg("--chdir").arg(worktree).arg("--");
        let script = self.wrapper_script(
            socket.is_some() && self.relay,
            egress::refused_path(worktree).as_deref(),
            phase,
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
        if let Some(runner) = &self.scope_runner {
            let mut scope = Command::new(runner);
            self.limits.scope_args(&mut scope);
            scope
                .arg(cmd.get_program())
                .args(cmd.get_args())
                .env_clear();
            for (key, value) in cmd.get_envs() {
                if let Some(value) = value {
                    scope.env(key, value);
                }
            }
            // Only the host launcher needs access to the user manager.
            for key in ["DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"] {
                if let Some(value) = std::env::var_os(key) {
                    scope.env(key, value);
                }
            }
            return scope;
        }
        cmd
    }
}

#[cfg(test)]
mod seeding;

#[cfg(test)]
mod tests;
