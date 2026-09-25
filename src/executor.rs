//! Kernel-owned command construction and the guarantees of each backend.
use crate::{
    config,
    egress::{Policy, Rule},
    sandbox::Sandbox,
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
}
impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bwrap => "bwrap",
            Self::Host => "host",
        }
    }
    pub fn guarantees(self) -> Guarantees {
        let isolated = self == Self::Bwrap;
        Guarantees {
            worktree_private: isolated,
            egress_bounded: isolated,
            credentials_seeded: isolated,
            checks_under_kernel_control: true,
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
pub trait Executor {
    fn guarantees(&self) -> Guarantees;
    fn command(
        &self,
        worktree: &Path,
        argv: &[String],
        env: &[(String, String)],
        egress: &Policy,
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
    ) -> Command {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(worktree)
            .env_clear()
            .envs(env.iter().cloned());
        cmd
    }
}
impl Executor for Sandbox {
    fn guarantees(&self) -> Guarantees {
        Backend::Bwrap.guarantees()
    }
    fn command(
        &self,
        worktree: &Path,
        argv: &[String],
        env: &[(String, String)],
        egress: &Policy,
    ) -> Command {
        self.command(worktree, argv, env, egress)
    }
}

/// Selection is installed only by the kernel when it reads trusted config.
/// Detection failures are retained so host repositories work without bwrap;
/// choosing bwrap still fails closed, never falling back to the host.
pub struct Execution {
    bwrap: Result<Sandbox, String>,
    backends: Mutex<BTreeMap<PathBuf, Backend>>,
}
impl Execution {
    pub fn detect(
        agent: &str,
        paths: &config::SandboxPaths,
        ro: Vec<PathBuf>,
        rw: Vec<PathBuf>,
        hosts: Vec<Rule>,
    ) -> anyhow::Result<Option<Self>> {
        if config::env("SANDBOX").as_deref() == Ok("0") {
            return Ok(None);
        }
        Ok(Some(Self {
            bwrap: Sandbox::detect(agent, paths, ro, rw, hosts).map_err(|e| format!("{e:#}")),
            backends: Mutex::new(BTreeMap::new()),
        }))
    }
    pub fn set_backend(&self, path: &Path, backend: Backend) {
        self.backends
            .lock()
            .unwrap()
            .insert(path.to_owned(), backend);
    }
    pub fn backend(&self, path: &Path) -> Backend {
        let backends = self.backends.lock().unwrap();
        path.ancestors()
            .find_map(|p| backends.get(p).copied())
            .unwrap_or_default()
    }
    pub fn guarantees(&self, path: &Path) -> Guarantees {
        match self.backend(path) {
            Backend::Host => Host.guarantees(),
            Backend::Bwrap => self
                .bwrap
                .as_ref()
                .map(Executor::guarantees)
                .unwrap_or_default(),
        }
    }
    pub fn command(&self, path: &Path, argv: &[String], env: &[(String, String)]) -> Command {
        let policy = self
            .bwrap
            .as_ref()
            .map(|s| s.policy_for(path))
            .unwrap_or_else(|_| Policy::new([]));
        if self.backend(path) == Backend::Host {
            return Host.command(path, argv, env, &policy);
        }
        match &self.bwrap {
            Ok(sb) => Executor::command(sb, path, argv, env, &policy),
            Err(error) => {
                let mut cmd = Command::new("/bin/sh");
                cmd.args([
                    "-c",
                    "printf '%s\\n' \"$1\" >&2; exit 127",
                    "forge-executor",
                    error,
                ]);
                cmd
            }
        }
    }
    pub fn set_egress(&self, path: &Path, rules: &[Rule]) {
        if let Ok(sb) = &self.bwrap {
            sb.set_egress(path, rules);
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
