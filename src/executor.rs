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
        ro: Vec<PathBuf>,
        rw: Vec<PathBuf>,
        hosts: Vec<Rule>,
    ) -> anyhow::Result<Option<Self>> {
        if config::env("SANDBOX").as_deref() == Ok("0") {
            return Ok(None);
        }
        Ok(Some(Self {
            fallback: default_backend(),
            bwrap: Sandbox::detect(agent, paths, ro, rw, hosts).map_err(|e| format!("{e:#}")),
            backends: Mutex::new(BTreeMap::new()),
            remotes: Mutex::new(BTreeMap::new()),
        }))
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
        match self.backend(path) {
            Backend::Host => Host.guarantees(),
            Backend::Ssh => Backend::Ssh.guarantees(),
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
        if self.backend(path) == Backend::Ssh {
            let remotes = self.remotes.lock().unwrap();
            let destination = path
                .ancestors()
                .find_map(|p| remotes.get(p))
                .expect("SSH executor configured");
            return ssh_command(destination, path, argv, env);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn execution_config_defaults_and_rejects_unknown_backends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forge.toml");
        let defaults = "[defaults]\nbase_branch = \"main\"\n";
        std::fs::write(&path, defaults).unwrap();
        assert_eq!(
            config::load_working(dir.path())
                .await
                .unwrap()
                .execution
                .declared,
            None
        );
        std::fs::write(
            &path,
            format!("{defaults}[execution]\nbackend = \"host\"\n"),
        )
        .unwrap();
        assert_eq!(
            config::load_working(dir.path())
                .await
                .unwrap()
                .execution
                .declared,
            Some(Backend::Host)
        );
        std::fs::write(&path, format!("{defaults}[execution]\nbackend = \"ssh\"\n")).unwrap();
        assert!(config::load_working(dir.path()).await.is_err());
    }

    #[test]
    fn host_preserves_argv_cwd_and_explicit_environment() {
        let dir = tempfile::tempdir().unwrap();
        let argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf '%s:%s' \"$VALUE\" \"$1\"; test \"$PWD\" = \"$EXPECTED\"".into(),
            "sh".into(),
            "two words".into(),
        ];
        let env = vec![
            ("VALUE".into(), "a value".into()),
            ("EXPECTED".into(), dir.path().display().to_string()),
        ];
        let output = Host
            .command(dir.path(), &argv, &env, &Policy::new([]))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"a value:two words");
    }

    #[test]
    fn unavailable_bwrap_never_falls_back_to_host() {
        let execution = Execution {
            bwrap: Err("bwrap unavailable".into()),
            fallback: Backend::Bwrap,
            backends: Mutex::new(BTreeMap::new()),
            remotes: Mutex::new(BTreeMap::new()),
        };
        let dir = tempfile::tempdir().unwrap();
        let argv = vec!["/bin/true".into()];
        assert!(
            !execution
                .command(dir.path(), &argv, &[])
                .output()
                .unwrap()
                .status
                .success()
        );
        execution.set_backend(dir.path(), Backend::Host);
        assert!(
            execution
                .command(dir.path(), &argv, &[])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert_eq!(execution.backend(&dir.path().join("child")), Backend::Host);
    }

    #[test]
    fn without_bwrap_an_undeclared_repository_runs_on_the_host() {
        let execution = Execution {
            bwrap: Err("bwrap not found".into()),
            fallback: Backend::Host,
            backends: Mutex::new(BTreeMap::new()),
            remotes: Mutex::new(BTreeMap::new()),
        };
        let dir = tempfile::tempdir().unwrap();
        let argv = vec!["/bin/sh".into(), "-c".into(), "true".into()];
        execution.configure(dir.path(), &config::Execution::default());
        assert_eq!(execution.backend(dir.path()), Backend::Host);
        assert!(!execution.guarantees(dir.path()).egress_bounded);
        assert!(
            execution
                .command(dir.path(), &argv, &[])
                .output()
                .unwrap()
                .status
                .success()
        );
        // Declaring bwrap still fails closed.
        execution.configure(
            dir.path(),
            &config::Execution {
                declared: Some(Backend::Bwrap),
                ..Default::default()
            },
        );
        assert!(
            !execution
                .command(dir.path(), &argv, &[])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
}
