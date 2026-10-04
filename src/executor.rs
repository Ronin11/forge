//! Kernel-owned command construction and the guarantees of each backend.
use crate::{
    config,
    egress::{Policy, Rule},
    sandbox::{Phase, Sandbox},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Bwrap,
    Host,
    Ssh,
}
impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bwrap => "bwrap",
            Self::Host => "host",
            Self::Ssh => "ssh",
        }
    }
    pub fn guarantees(self) -> Guarantees {
        let isolated = self == Self::Bwrap;
        Guarantees {
            worktree_private: isolated,
            egress_bounded: isolated,
            credentials_seeded: isolated,
            checks_under_kernel_control: self != Self::Ssh,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Guarantees {
    pub worktree_private: bool,
    pub egress_bounded: bool,
    pub credentials_seeded: bool,
    pub checks_under_kernel_control: bool,
}
/// `Execution::guarantees_for` for a process that never resolved a sandbox
/// (a `forge add`, which only files the task): `FORGE_SANDBOX=0` means
/// every launch is on the host, and a declared bwrap without the binary
/// guarantees nothing.
pub fn guarantees_unresolved(cfg: &config::Execution) -> (Backend, Guarantees) {
    if config::env("SANDBOX").as_deref() == Ok("0") {
        return (Backend::Host, Backend::Host.guarantees());
    }
    let backend = cfg.declared.unwrap_or_else(default_backend);
    let available = backend != Backend::Bwrap || crate::sandbox::resolve_binary("bwrap").is_ok();
    let guarantees = if available {
        backend.guarantees()
    } else {
        Guarantees::default()
    };
    (backend, guarantees)
}

pub trait Executor {
    fn guarantees(&self) -> Guarantees;
    fn command(
        &self,
        worktree: &Path,
        argv: &[String],
        env: &[(String, String)],
        egress: &Policy,
        phase: Phase,
    ) -> Command;
}
pub struct Host;
impl Executor for Host {
    fn guarantees(&self) -> Guarantees {
        Backend::Host.guarantees()
    }
    fn command(
        &self,
        worktree: &Path,
        argv: &[String],
        env: &[(String, String)],
        _: &Policy,
        _: Phase,
    ) -> Command {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(worktree)
            .env_clear()
            .envs(env.iter().cloned());
        cmd
    }
}
/// Synchronize a private scratch clone, preserve argv and stdin, and retrieve
/// edits (including commits) even when the remote process fails.
fn ssh_command(
    destination: &str,
    worktree: &Path,
    argv: &[String],
    env: &[(String, String)],
) -> Command {
    use crate::sandbox::shell_quote;
    let local = worktree.to_string_lossy();
    let relocate = |value: &str| {
        if value == local.as_ref() {
            ".".to_owned()
        } else if let Some(relative) = value.strip_prefix(&format!("{local}/")) {
            format!("./{relative}")
        } else {
            value.to_owned()
        }
    };
    let args = argv
        .iter()
        .map(|a| shell_quote(&relocate(a)))
        .collect::<Vec<_>>()
        .join(" ");
    let vars = env
        .iter()
        .map(|(k, v)| shell_quote(&format!("{k}={}", relocate(v))))
        .collect::<Vec<_>>()
        .join(" ");
    let remote = format!("env -i {vars} {args}");
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        r#"
set -eu
dest=$1
tree=$2
run=$3
scratch=$(ssh "$dest" 'mktemp -d /tmp/forge-executor.XXXXXXXXXX' </dev/null)
case "$scratch" in /tmp/forge-executor.*) ;; *) exit 125 ;; esac
case "$scratch" in *[!a-zA-Z0-9/._-]*) exit 125 ;; esac
trap 'ssh "$dest" "rm -rf -- $scratch" </dev/null >/dev/null 2>&1 || true' EXIT
rsync -a --delete "$tree/" "$dest:$scratch/" </dev/null
status=0
ssh "$dest" "cd '$scratch' && $run" || status=$?
rsync -a --delete "$dest:$scratch/" "$tree/" </dev/null
exit "$status"
"#,
        "forge-ssh",
        destination,
        &local,
        &remote,
    ]);
    command
}

/// The backend a repository that names none runs on: bwrap, or host on a
/// machine without bwrap at all (macOS), where refusing to run would leave
/// Forge unusable. A repository that declares `backend = "bwrap"` still
/// fails closed without it; `forge doctor` warns what host forgoes.
pub fn default_backend() -> Backend {
    if crate::sandbox::resolve_binary("bwrap").is_ok() {
        Backend::Bwrap
    } else {
        Backend::Host
    }
}

/// Remote CLIs are resolved by the remote shell, never by local mise.
pub fn agent_bin(execution: Option<&Execution>, path: &Path, name: String) -> String {
    if execution.is_some_and(|e| e.backend(path) == Backend::Ssh) {
        name
    } else {
        crate::agent::real_bin(&name)
    }
}

/// Selection is installed only by the kernel when it reads trusted config.
/// Detection failures are retained so host repositories work without bwrap;
/// choosing bwrap still fails closed, never falling back to the host.
pub struct Execution {
    bwrap: Result<Sandbox, String>,
    /// Where a path with no declared backend runs (`default_backend`).
    fallback: Backend,
    backends: Mutex<BTreeMap<PathBuf, Backend>>,
    remotes: Mutex<BTreeMap<PathBuf, String>>,
}
impl Execution {
    pub fn detect(
        agent: &str,
        paths: &config::SandboxPaths,
        forge_home: PathBuf,
        ro: Vec<PathBuf>,
        rw: Vec<PathBuf>,
    ) -> anyhow::Result<Option<Self>> {
        if config::env("SANDBOX").as_deref() == Ok("0") {
            return Ok(None);
        }
        Ok(Some(Self {
            fallback: default_backend(),
            bwrap: Sandbox::detect(agent, paths, forge_home, ro, rw).map_err(|e| format!("{e:#}")),
            backends: Mutex::new(BTreeMap::new()),
            remotes: Mutex::new(BTreeMap::new()),
        }))
    }
    /// An execution that runs everything through `sandbox`.
    #[cfg(test)]
    pub(crate) fn bwrap_only(sandbox: Sandbox) -> Self {
        Self {
            bwrap: Ok(sandbox),
            fallback: Backend::Bwrap,
            backends: Mutex::new(BTreeMap::new()),
            remotes: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn set_backend(&self, path: &Path, backend: Backend) {
        self.backends
            .lock()
            .unwrap()
            .insert(path.to_owned(), backend);
    }
    pub fn configure(&self, path: &Path, cfg: &config::Execution) {
        match cfg.declared {
            Some(backend) => self.set_backend(path, backend),
            None => {
                self.backends.lock().unwrap().remove(path);
            }
        }
        if cfg.declared == Some(Backend::Ssh) {
            self.remotes.lock().unwrap().insert(
                path.to_owned(),
                cfg.ssh_destination().expect("validated execution config"),
            );
        }
    }
    pub fn backend(&self, path: &Path) -> Backend {
        let backends = self.backends.lock().unwrap();
        path.ancestors()
            .find_map(|p| backends.get(p).copied())
            .unwrap_or(self.fallback)
    }
    pub fn guarantees(&self, path: &Path) -> Guarantees {
        self.guarantees_of(self.backend(path))
    }
    fn guarantees_of(&self, backend: Backend) -> Guarantees {
        match backend {
            Backend::Host => Host.guarantees(),
            Backend::Ssh => Backend::Ssh.guarantees(),
            Backend::Bwrap => self
                .bwrap
                .as_ref()
                .map(|_| Backend::Bwrap.guarantees())
                .unwrap_or_default(),
        }
    }
    /// The backend a repository declaring `cfg` would run on, and what it
    /// guarantees here, before any worktree exists.
    pub fn guarantees_for(&self, cfg: &config::Execution) -> (Backend, Guarantees) {
        let backend = cfg.declared.unwrap_or(self.fallback);
        (backend, self.guarantees_of(backend))
    }
    pub fn command(
        &self,
        path: &Path,
        argv: &[String],
        env: &[(String, String)],
        phase: Phase,
    ) -> anyhow::Result<Command> {
        self.command_under(path, argv, env, None, phase)
    }
    /// An explicit policy replaces inherited model, repository and granted hosts.
    pub fn command_under(
        &self,
        path: &Path,
        argv: &[String],
        env: &[(String, String)],
        egress: Option<&Policy>,
        phase: Phase,
    ) -> anyhow::Result<Command> {
        let policy = self
            .bwrap
            .as_ref()
            .map(|s| s.policy_for(path))
            .unwrap_or_else(|_| Policy::new([]));
        let policy = egress.unwrap_or(&policy);
        if self.backend(path) == Backend::Ssh {
            let remotes = self.remotes.lock().unwrap();
            let destination = path
                .ancestors()
                .find_map(|p| remotes.get(p))
                .expect("SSH executor configured");
            return Ok(ssh_command(destination, path, argv, env));
        }
        if self.backend(path) == Backend::Host {
            return Ok(Host.command(path, argv, env, policy, phase));
        }
        let sb = self
            .bwrap
            .as_ref()
            .map_err(|error| crate::egress::SocketError::Failed(anyhow::anyhow!("{error}")))?;
        let socket = match sb.proxies.socket_for(policy) {
            Ok(socket) => Some(socket),
            Err(crate::egress::SocketError::NoRuntime) => None,
            Err(error @ crate::egress::SocketError::Failed(_)) => return Err(error.into()),
        };
        Ok(sb.command(path, argv, env, socket.as_deref(), phase))
    }
    /// What a launch in `path` with `env` does before its `command` is
    /// built: a bwrap launch seeds its private logins (see
    /// `Sandbox::prepare`); any other has nothing to prepare.
    pub async fn prepare(&self, path: &Path, env: &[(String, String)], phase: Phase) {
        if let Ok(sb) = &self.bwrap
            && self.backend(path) == Backend::Bwrap
        {
            sb.prepare(path, env, phase).await;
        }
    }
    /// After a launch in `path`: write the attempt's private login back over
    /// the host file if it refreshed it (see `login`). Only a bwrap launch
    /// has a private copy; whether a write-back happened.
    pub async fn write_back_login(&self, shape: &crate::login::Shape, path: &Path) -> bool {
        match &self.bwrap {
            Ok(sb) if self.backend(path) == Backend::Bwrap => {
                sb.write_back_login(shape, path).await
            }
            _ => false,
        }
    }
    /// `write_back_login` for every task's private login beside `path`, for
    /// a caller holding the login's lock.
    pub fn write_back_siblings_locked(&self, path: &Path) {
        if let Ok(sb) = &self.bwrap {
            sb.write_back_siblings_locked(path);
        }
    }
    pub fn set_egress(&self, path: &Path, rules: &[Rule]) {
        if let Ok(sb) = &self.bwrap {
            sb.set_egress(path, rules);
        }
    }
    pub fn set_provider_hosts(&self, path: &Path, rules: &[Rule]) {
        if let Ok(sb) = &self.bwrap {
            sb.set_provider_hosts(path, rules);
        }
    }
    pub fn set_target_dir(&self, path: &Path, target: PathBuf) {
        if let Ok(sb) = &self.bwrap {
            sb.set_target_dir(path, target);
        }
    }

    pub fn set_cache_dir(&self, path: &Path, dir: PathBuf) {
        if let Ok(sb) = &self.bwrap {
            sb.set_cache_dir(path, dir);
        }
    }
    pub fn grant_host(&self, path: &Path, rule: Rule) -> bool {
        self.bwrap
            .as_ref()
            .is_ok_and(|sb| sb.grant_host(path, rule))
    }
    pub fn grant_ro(&self, path: &Path, dir: PathBuf) -> bool {
        self.bwrap.as_ref().is_ok_and(|sb| sb.grant_ro(path, dir))
    }
}

#[cfg(test)]
mod tests;
