//! The process-wide context: where things live, the store, the operator's
//! budget, the sandbox, and the reporter. Built once and shared.

use crate::agent;
use crate::config::{self, Budget};
use crate::executor::Execution;
use crate::report::Reporter;
use crate::store::{Store, Task};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The provider a step under `role` actually runs (see `config::ROLES`):
/// the task's own `--provider` (set, so every role for that task wins),
/// else this task's `[measure] explore` draw for `role` (see
/// `queue::assign_explore`), else the project's `[roles]` override for
/// `role`, else the operator's `[roles]` table, else the built-in
/// "anthropic". `task_provider` is `""` when the task named no
/// `--provider`.
pub fn resolve_provider<'a>(
    providers: &'a BTreeMap<String, agent::Provider>,
    operator_roles: &BTreeMap<String, String>,
    project_roles: &BTreeMap<String, String>,
    explore: &BTreeMap<String, String>,
    task_provider: &str,
    role: &str,
) -> Result<&'a agent::Provider> {
    resolve_provider_routed(
        providers,
        operator_roles,
        project_roles,
        explore,
        task_provider,
        role,
    )
    .map(|(p, _)| p)
}

/// `resolve_provider`, also naming which layer decided: `"flag"` (the
/// task's own `--provider`), `"experiment"` (an explore draw, see
/// `queue::assign_explore`), `"project"`, `"operator"`, or `"default"`
/// (the built-in "anthropic", nothing else named a provider for `role`).
/// Recorded on `Task::routing` (see docs/ECONOMIST.md, "The routing
/// record") so the record shows why a step ran where it did.
pub fn resolve_provider_routed<'a>(
    providers: &'a BTreeMap<String, agent::Provider>,
    operator_roles: &BTreeMap<String, String>,
    project_roles: &BTreeMap<String, String>,
    explore: &BTreeMap<String, String>,
    task_provider: &str,
    role: &str,
) -> Result<(&'a agent::Provider, &'static str)> {
    // A task's --provider (explicit or drawn by explore) routes the work,
    // never the judge: the supervisor rules on the record and keeps the
    // operator's or the project's provider for that role (task 309's
    // supervisor ran on the task's local model and tried to pull "opus"
    // from ollama).
    let (name, source) = if !task_provider.is_empty() && role != "supervisor" {
        (task_provider, "flag")
    } else if role != "supervisor"
        && let Some(p) = explore.get(role)
    {
        (p.as_str(), "experiment")
    } else if let Some(p) = project_roles.get(role) {
        (p.as_str(), "project")
    } else if let Some(p) = operator_roles.get(role) {
        (p.as_str(), "operator")
    } else {
        ("anthropic", "default")
    };
    let provider = providers.get(name).with_context(|| {
        format!("unknown provider {name:?} for role {role:?}; see `forge providers` for what is configured")
    })?;
    Ok((provider, source))
}

#[derive(Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub worktrees: PathBuf,
    pub logs: PathBuf,
}

impl Paths {
    /// The directory `resolve` would use, without creating anything: what
    /// `forge init` reports and writes into before `resolve`'s own
    /// `create_dir_all` calls would otherwise hide whether it already
    /// existed.
    pub fn compute_home() -> Result<PathBuf> {
        if let Ok(p) = config::env("HOME") {
            return Ok(PathBuf::from(p));
        }
        let base = match std::env::var("XDG_DATA_HOME") {
            Ok(p) => PathBuf::from(p),
            Err(_) => PathBuf::from(std::env::var("HOME").context("HOME is not set")?)
                .join(".local/share"),
        };
        let new = base.join("forge");
        let old = base.join("forge2");
        Ok(if !new.exists() && old.exists() {
            old
        } else {
            new
        })
    }

    /// `Paths` for a given `home`, creating `worktrees` and `logs` under it.
    pub fn for_home(home: PathBuf) -> Result<Paths> {
        let p = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&p.worktrees)?;
        std::fs::create_dir_all(&p.logs)?;
        Ok(p)
    }

    /// `FORGE_HOME` (`FORGE2_HOME` for one release, see `config::env`), else
    /// `$XDG_DATA_HOME/forge`, else `~/.local/share/forge` — falling back to
    /// `~/.local/share/forge2` when the new default does not exist yet but
    /// the old one does, so a machine that has never set `FORGE_HOME` keeps
    /// reading its existing data until the operator moves it by hand (see
    /// `legacy_home_migration`, which `forge doctor` uses to say so).
    pub fn resolve() -> Result<Paths> {
        Self::for_home(Self::compute_home()?)
    }
}

/// The exact `mv` `forge doctor` tells the operator to run when nothing
/// names a home explicitly (`FORGE_HOME`, `FORGE2_HOME`, `XDG_DATA_HOME`
/// all unset) and `Paths::resolve` fell back to the pre-rename default
/// (`~/.local/share/forge2`) because the new one does not exist yet:
/// `(new, old)`. `None` once the operator has moved it, set one of those
/// variables, or never had the old directory at all.
pub fn legacy_home_migration() -> Option<(PathBuf, PathBuf)> {
    if config::env("HOME").is_ok() || std::env::var("XDG_DATA_HOME").is_ok() {
        return None;
    }
    let home = std::env::var("HOME").ok()?;
    let base = PathBuf::from(home).join(".local/share");
    let new = base.join("forge");
    let old = base.join("forge2");
    (!new.exists() && old.exists()).then_some((new, old))
}

pub struct Forge {
    pub paths: Paths,
    pub store: Store,
    pub budget: Budget,
    pub supervisor: config::Supervisor,
    pub early_ending: config::EarlyEnding,
    pub measure: config::Measure,
    pub intake: config::Intake,
    /// `[trust.<level>]`: the policy each of the three trust levels a
    /// task can carry is judged against at enqueue (see
    /// `queue::apply_trust_policy`).
    pub trust: config::TrustPolicies,
    /// Agent backends by name, the built-in "anthropic" always present
    /// (see `config::load_home`).
    pub providers: std::collections::BTreeMap<String, agent::Provider>,
    /// Every role's default provider name (see `config::ROLES`).
    pub roles: std::collections::BTreeMap<String, String>,
    /// A project's secrets, by project name (see `config::load_home`).
    pub project_secrets: BTreeMap<String, BTreeMap<String, String>>,
    /// `[environment]`: what a failed attempt's environment need may be
    /// granted automatically (see `environment`).
    pub environment: crate::environment::Policy,
    pub sandbox: Option<Execution>,
    /// Grants already applied per worktree when there is no sandbox to
    /// remember them, so each applies once here too.
    applied: std::sync::Mutex<Vec<(PathBuf, crate::environment::Grant)>>,
    pub report: Reporter,
}

impl Forge {
    /// `need_agent` resolves the sandbox and agent binary, which only the
    /// commands that run attempts need. `prefix` tags output with task ids.
    pub fn open(need_agent: bool, prefix: bool) -> Result<Forge> {
        let paths = Paths::resolve()?;
        let store = Store::open(&paths.home.join("forge.db"))?;
        config::ensure_home_config(&paths.home)?;
        Forge::build(paths, store, need_agent, prefix)
    }

    /// `FORGE_HOME/config.toml` loaded and validated, and the sandbox
    /// resolved against it when `need_agent`: everything `open` does once
    /// the paths and store are in hand.
    fn build(paths: Paths, store: Store, need_agent: bool, prefix: bool) -> Result<Forge> {
        let home = config::load_home(&paths.home)?;
        let sandbox = if need_agent {
            // Forge's own tools (forge-repomap) live beside the binary.
            let mut extra_ro = Vec::new();
            if let Ok(exe) = std::env::current_exe()
                && let Some(dir) = exe.parent()
            {
                extra_ro.push(dir.to_path_buf());
            }
            Execution::detect(
                &agent::agent_bin(),
                &home.sandbox,
                extra_ro,
                Vec::new(),
                crate::egress::model_rules(&home.providers),
            )?
        } else {
            None
        };
        let report = Reporter::new(prefix, Some(paths.home.join("events.jsonl")));
        Ok(Forge {
            paths,
            store,
            budget: home.budget,
            supervisor: home.supervisor,
            early_ending: home.early_ending,
            measure: home.measure,
            intake: home.intake,
            trust: home.trust,
            providers: home.providers,
            roles: home.roles,
            project_secrets: home.project_secrets,
            environment: home.environment,
            sandbox,
            applied: Default::default(),
            report,
        })
    }

    /// A fresh `Forge` over the same home, with `config.toml` re-read and
    /// re-validated exactly as `open` does at start (the worker's reload
    /// between claims, `crate::reload`). Its own store connection, so the
    /// `Forge` an attempt in flight holds is never touched.
    pub fn reopen(&self) -> Result<Forge> {
        let store = Store::open(&self.paths.home.join("forge.db"))?;
        Forge::build(
            self.paths.clone(),
            store,
            self.sandbox.is_some(),
            self.report.prefixed(),
        )
    }

    /// For commands that already hold the paths and store (doctor).
    pub fn open_with(paths: Paths, store: Store) -> Result<Forge> {
        let home = config::load_home(&paths.home)?;
        let report = Reporter::new(false, Some(paths.home.join("events.jsonl")));
        Ok(Forge {
            paths,
            store,
            budget: home.budget,
            supervisor: home.supervisor,
            early_ending: home.early_ending,
            measure: home.measure,
            intake: home.intake,
            trust: home.trust,
            providers: home.providers,
            roles: home.roles,
            project_secrets: home.project_secrets,
            environment: home.environment,
            sandbox: None,
            applied: Default::default(),
            report,
        })
    }

    /// Tell the sandbox what a repository's config lets attempts in
    /// `worktree` reach besides the model endpoint. No-op unsandboxed.
    pub fn allow_egress(&self, worktree: &Path, cfg: &config::Config, trust: crate::store::Trust) {
        if let Some(sandbox) = &self.sandbox {
            sandbox.configure(worktree, &cfg.execution);
            // A level whose egress is `model` reaches the model endpoints
            // alone, whatever the repository declares.
            match self.trust_policy(trust).egress {
                config::TrustEgress::Model => sandbox.set_egress(worktree, &[]),
                config::TrustEgress::Declared => sandbox.set_egress(worktree, &cfg.egress),
            }
        }
    }

    /// Apply what the environment policy grants for `need` to attempts in
    /// `worktree`: the host on top of its egress, or the cache path
    /// read-only. `None` when the policy does not cover the need, the
    /// trust level reaches the model endpoints alone, there is no sandbox,
    /// or the grant was already applied (a re-run would fail the same way).
    pub fn grant_environment(
        &self,
        worktree: &Path,
        need: &crate::environment::Need,
        trust: crate::store::Trust,
    ) -> Option<crate::environment::Grant> {
        let grant = self.environment.covers(need)?;
        self.apply_grant(worktree, grant, trust)
    }

    /// Open `grant` for attempts in `worktree`, whoever allowed it; `None`
    /// when it cannot be applied or was applied before.
    pub fn apply_grant(
        &self,
        worktree: &Path,
        grant: crate::environment::Grant,
        trust: crate::store::Trust,
    ) -> Option<crate::environment::Grant> {
        use crate::environment::Grant;
        let declared = matches!(
            self.trust_policy(trust).egress,
            config::TrustEgress::Declared
        );
        // Unsandboxed there is nothing to open, so the grant is only
        // remembered: the first time it is seen the run repeats, the
        // second it would fail the same way.
        let fresh = match (&self.sandbox, &grant) {
            (_, Grant::Host(_)) if !declared => false,
            (Some(sb), Grant::Host(h)) => {
                sb.grant_host(worktree, crate::egress::Rule::parse(h).ok()?)
            }
            (Some(sb), Grant::ReadOnly(p)) => sb.grant_ro(worktree, p.clone()),
            (None, _) => {
                let mut seen = self.applied.lock().unwrap_or_else(|e| e.into_inner());
                let key = (worktree.to_path_buf(), grant.clone());
                !seen.contains(&key) && {
                    seen.push(key);
                    true
                }
            }
        };
        fresh.then_some(grant)
    }

    /// Where `repo`'s operations may cache what they compute
    /// (`FORGE_CACHE_DIR`): `paths.home/cache/<hash of repo's path>`, so
    /// two repositories never share a directory and one cannot poison or
    /// read what the other cached.
    pub fn cache_dir(&self, repo: &Path) -> PathBuf {
        let key = crate::job::sha256_hex(repo.to_string_lossy().as_bytes());
        self.paths.home.join("cache").join(&key[..16])
    }

    /// Tell the sandbox where attempts in `worktree` (this task's clone of
    /// `repo`) may reach their cache. No-op unsandboxed.
    pub fn declare_cache(&self, worktree: &Path, repo: &Path) {
        if let Some(sandbox) = &self.sandbox {
            sandbox.set_cache_dir(worktree, self.cache_dir(repo));
        }
    }

    /// The policy `[trust.<level>]` sets for `level`.
    pub fn trust_policy(&self, level: crate::store::Trust) -> &config::TrustPolicy {
        match level {
            crate::store::Trust::Operator => &self.trust.operator,
            crate::store::Trust::Contact => &self.trust.contact,
            crate::store::Trust::Public => &self.trust.public,
        }
    }

    /// Whether a task at `level` may start on the backend a repository
    /// declaring `cfg` would run on: `Err` names the backend and the level.
    /// Without a resolved sandbox, `FORGE_SANDBOX=0` puts every launch on
    /// the host.
    pub fn egress_gate(
        &self,
        cfg: &config::Config,
        level: crate::store::Trust,
    ) -> Result<(), String> {
        let (backend, guarantees) = match &self.sandbox {
            Some(execution) => execution.guarantees_for(&cfg.execution),
            None => crate::executor::guarantees_unresolved(&cfg.execution),
        };
        egress_gate(level, self.trust_policy(level), backend, guarantees)
    }

    pub fn sandboxed(&self, worktree: &Path) -> bool {
        self.sandbox
            .as_ref()
            .is_some_and(|s| s.backend(worktree) == crate::executor::Backend::Bwrap)
    }

    pub fn execution_inputs(&self, worktree: &Path) -> crate::audit::Inputs {
        let backend = self
            .sandbox
            .as_ref()
            .map(|s| s.backend(worktree))
            .unwrap_or(crate::executor::Backend::Host);
        crate::audit::Inputs {
            executor: backend.as_str().to_string(),
            guarantees: self
                .sandbox
                .as_ref()
                .map(|s| s.guarantees(worktree))
                .unwrap_or_else(|| backend.guarantees()),
            ..Default::default()
        }
    }

    /// A task's project, when it has one and the row still exists. Errors
    /// reading the store are swallowed to `None`: every caller of this
    /// treats a project as an optional layer, never a hard dependency.
    fn task_project(&self, t: &crate::store::Task) -> Option<crate::store::Project> {
        t.project
            .as_ref()
            .and_then(|p| self.store.project(p).ok().flatten())
    }

    /// The provider `t`'s step under `role` actually runs: see
    /// `resolve_provider`.
    pub fn effective_provider(&self, t: &Task, role: &str) -> Result<&agent::Provider> {
        self.effective_provider_routed(t, role).map(|(p, _)| p)
    }

    /// `effective_provider`, also naming which layer decided: see
    /// `resolve_provider_routed`.
    pub fn effective_provider_routed(
        &self,
        t: &Task,
        role: &str,
    ) -> Result<(&agent::Provider, &'static str)> {
        let project_roles = self
            .task_project(t)
            .map(|p| p.role_providers)
            .unwrap_or_default();
        resolve_provider_routed(
            &self.providers,
            &self.roles,
            &project_roles,
            &t.explore,
            &t.provider,
            role,
        )
    }

    /// The per-task budget cap that actually applies: the task's own
    /// `--budget`, else its project's `per_task_usd` default, else the
    /// operator's (see docs/PROJECTS.md, "Configuration layering").
    pub fn effective_per_task_usd(&self, t: &crate::store::Task) -> f64 {
        t.budget_usd
            .or_else(|| self.task_project(t).and_then(|p| p.per_task_usd))
            .unwrap_or(self.budget.per_task_usd)
    }

    /// The supervisor settings that actually apply: the operator's, with
    /// the task's project's model and per-lineage cap layered on top.
    pub fn effective_supervisor(&self, t: &crate::store::Task) -> config::Supervisor {
        let mut cfg = self.supervisor.clone();
        if let Some(p) = self.task_project(t) {
            if let Some(model) = p.supervisor_model {
                cfg.model = model;
            }
            if let Some(per_lineage) = p.supervisor_per_lineage {
                cfg.per_lineage = per_lineage as u32;
            }
        }
        cfg
    }

    /// The protected paths that actually apply: the repository's own
    /// (`repo_protected`, from `forge.toml`) plus its project's extra
    /// ones, deduplicated.
    pub fn effective_protected(
        &self,
        t: &crate::store::Task,
        repo_protected: &[String],
    ) -> Vec<String> {
        let mut out = repo_protected.to_vec();
        if let Some(extra) = self.task_project(t).and_then(|p| p.protected) {
            for e in extra {
                if !out.contains(&e) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// The write scope a task's own attempts inherit when a workflow
    /// directive does not already narrow it: its project's scope for its
    /// own repository, or unrestricted when the project does not list it.
    pub fn effective_paths(&self, t: &crate::store::Task) -> Vec<String> {
        let Some(name) = &t.project else {
            return Vec::new();
        };
        let Ok(repos) = self.store.project_repos(name) else {
            return Vec::new();
        };
        let Some(scope_json) = repos
            .into_iter()
            .find(|r| r.repo == t.repo)
            .and_then(|r| r.scope)
        else {
            return Vec::new();
        };
        serde_json::from_str(&scope_json).unwrap_or_default()
    }
}

/// Each trust level's cost caps as `forge doctor` prints them; the
/// operator's per-task cap falls to `[budget]`.
pub fn describe_trust_caps(trust: &config::TrustPolicies, budget: &Budget) -> String {
    let usd = |v: Option<f64>| v.map_or("none".to_string(), |v| format!("${v:.2}"));
    let levels = [
        ("operator", &trust.operator, Some(budget.per_task_usd)),
        ("contact", &trust.contact, None),
        ("public", &trust.public, None),
    ];
    levels
        .map(|(name, p, fallback)| {
            let (task, initiative) = (usd(p.per_task_usd.or(fallback)), usd(p.per_initiative_usd));
            format!("{name} {task} a task / {initiative} an initiative")
        })
        .join(", ")
}

/// Prefix of the reason a task is refused (at enqueue) or blocked (at
/// claim) by `egress_gate`; the worker never hands such a block to the
/// supervisor, since no answer changes it.
pub const EGRESS_REFUSAL: &str = "refused to start: ";

/// Whether a task at `level` may start on `backend`: a level that is not
/// operator, or whose `[trust]` egress is `model`, promises bounded egress
/// and a private worktree, and only a backend that reports both keeps the
/// promise, unless the level opts out with `allow_unsandboxed`.
pub fn egress_gate(
    level: crate::store::Trust,
    policy: &config::TrustPolicy,
    backend: crate::executor::Backend,
    guarantees: crate::executor::Guarantees,
) -> Result<(), String> {
    let restricted =
        level != crate::store::Trust::Operator || policy.egress == config::TrustEgress::Model;
    if !restricted
        || policy.allow_unsandboxed
        || (guarantees.egress_bounded && guarantees.worktree_private)
    {
        return Ok(());
    }
    Err(format!(
        "{EGRESS_REFUSAL}trust {} (egress {}) needs a backend with egress_bounded and \
         worktree_private, but backend {} reports egress_bounded={}, worktree_private={}; \
         run it on bwrap, or set allow_unsandboxed = true under [trust.{}] in config.toml",
        level.as_str(),
        policy.egress.as_str(),
        backend.as_str(),
        guarantees.egress_bounded,
        guarantees.worktree_private,
        level.as_str(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{Backend, Guarantees};
    use crate::store::Trust;

    fn policy(egress: config::TrustEgress, allow_unsandboxed: bool) -> config::TrustPolicy {
        config::TrustPolicy {
            per_task_usd: None,
            per_initiative_usd: None,
            workflows: None,
            allow_protected: true,
            egress,
            per_day: None,
            auto_land: true,
            allow_unsandboxed,
        }
    }

    #[test]
    fn the_gate_refuses_a_restricted_level_on_every_backend_that_does_not_bound_egress() {
        for backend in [Backend::Host, Backend::Ssh] {
            for level in [Trust::Contact, Trust::Public] {
                let p = policy(config::TrustEgress::Declared, false);
                let err = egress_gate(level, &p, backend, backend.guarantees()).unwrap_err();
                assert!(err.starts_with(EGRESS_REFUSAL), "{err}");
                assert!(err.contains(backend.as_str()), "{err}");
                assert!(err.contains(level.as_str()), "{err}");
            }
            let p = policy(config::TrustEgress::Model, false);
            let err = egress_gate(Trust::Operator, &p, backend, backend.guarantees()).unwrap_err();
            assert!(
                err.contains(backend.as_str()) && err.contains("operator"),
                "{err}"
            );
        }
    }

    #[test]
    fn the_gate_lets_bwrap_carry_every_level() {
        let g = Backend::Bwrap.guarantees();
        for level in [Trust::Operator, Trust::Contact, Trust::Public] {
            for egress in [config::TrustEgress::Model, config::TrustEgress::Declared] {
                assert!(egress_gate(level, &policy(egress, false), Backend::Bwrap, g).is_ok());
            }
        }
    }

    #[test]
    fn the_gate_refuses_bwrap_when_it_is_unavailable() {
        let p = policy(config::TrustEgress::Model, false);
        let err =
            egress_gate(Trust::Public, &p, Backend::Bwrap, Guarantees::default()).unwrap_err();
        assert!(err.contains("bwrap") && err.contains("public"), "{err}");
    }

    #[test]
    fn the_gate_leaves_an_operator_task_with_declared_egress_alone_on_any_backend() {
        let p = policy(config::TrustEgress::Declared, false);
        for backend in [Backend::Bwrap, Backend::Host, Backend::Ssh] {
            assert!(egress_gate(Trust::Operator, &p, backend, backend.guarantees()).is_ok());
        }
    }

    #[test]
    fn allow_unsandboxed_opts_a_level_out_of_the_gate() {
        let p = policy(config::TrustEgress::Model, true);
        for backend in [Backend::Host, Backend::Ssh] {
            assert!(egress_gate(Trust::Public, &p, backend, backend.guarantees()).is_ok());
        }
    }

    fn providers(names: &[&str]) -> BTreeMap<String, agent::Provider> {
        names
            .iter()
            .map(|n| {
                (
                    n.to_string(),
                    agent::Provider {
                        name: n.to_string(),
                        ..agent::Provider::default()
                    },
                )
            })
            .collect()
    }

    #[test]
    fn an_unsandboxed_grant_applies_once_per_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("forge.db")).unwrap();
        let paths = Paths {
            home: dir.path().to_path_buf(),
            worktrees: dir.path().join("worktrees"),
            logs: dir.path().join("logs"),
        };
        let f = Forge::open_with(paths, store).unwrap();
        assert!(f.sandbox.is_none());
        let g = crate::environment::Grant::ReadOnly(PathBuf::from("/home/x/.cache/pw"));
        let t = crate::store::Trust::Operator;
        assert_eq!(
            f.apply_grant(Path::new("/w/1"), g.clone(), t),
            Some(g.clone())
        );
        assert_eq!(f.apply_grant(Path::new("/w/1"), g.clone(), t), None);
        assert_eq!(f.apply_grant(Path::new("/w/2"), g.clone(), t), Some(g));
    }

    #[test]
    fn a_tasks_provider_never_routes_the_supervisor() {
        let providers = providers(&["anthropic", "devhome"]);
        let p = resolve_provider(
            &providers,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            "devhome",
            "code",
        )
        .unwrap();
        assert_eq!(p.name, "devhome");
        let s = resolve_provider(
            &providers,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            "devhome",
            "supervisor",
        )
        .unwrap();
        assert_eq!(s.name, "anthropic", "the judge keeps its own provider");
    }

    #[test]
    fn resolve_provider_falls_to_anthropic_with_nothing_configured() {
        let providers = providers(&["anthropic"]);
        let p = resolve_provider(
            &providers,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            "",
            "code",
        )
        .unwrap();
        assert_eq!(p.name, "anthropic");
    }

    #[test]
    fn resolve_provider_uses_the_operators_role_table() {
        let providers = providers(&["anthropic", "devhome"]);
        let operator_roles: BTreeMap<String, String> =
            [("code".to_string(), "devhome".to_string())].into();
        let p = resolve_provider(
            &providers,
            &operator_roles,
            &BTreeMap::new(),
            &BTreeMap::new(),
            "",
            "code",
        )
        .unwrap();
        assert_eq!(p.name, "devhome");
        // A role the operator did not name still falls to anthropic.
        let p = resolve_provider(
            &providers,
            &operator_roles,
            &BTreeMap::new(),
            &BTreeMap::new(),
            "",
            "tests",
        )
        .unwrap();
        assert_eq!(p.name, "anthropic");
    }

    #[test]
    fn resolve_provider_the_projects_role_wins_over_the_operators() {
        let providers = providers(&["anthropic", "devhome", "openai"]);
        let operator_roles: BTreeMap<String, String> =
            [("code".to_string(), "devhome".to_string())].into();
        let project_roles: BTreeMap<String, String> =
            [("code".to_string(), "openai".to_string())].into();
        let p = resolve_provider(
            &providers,
            &operator_roles,
            &project_roles,
            &BTreeMap::new(),
            "",
            "code",
        )
        .unwrap();
        assert_eq!(p.name, "openai");
    }

    #[test]
    fn resolve_provider_explore_wins_over_the_project_and_operator_but_not_the_tasks_own_flag() {
        let providers = providers(&["anthropic", "devhome", "openai"]);
        let operator_roles: BTreeMap<String, String> =
            [("code".to_string(), "devhome".to_string())].into();
        let project_roles: BTreeMap<String, String> =
            [("code".to_string(), "openai".to_string())].into();
        let explore: BTreeMap<String, String> =
            [("code".to_string(), "anthropic".to_string())].into();
        let p = resolve_provider(
            &providers,
            &operator_roles,
            &project_roles,
            &explore,
            "",
            "code",
        )
        .unwrap();
        assert_eq!(
            p.name, "anthropic",
            "explore wins over project and operator"
        );
        // An explicit task provider still wins over an explore draw.
        let p = resolve_provider(
            &providers,
            &operator_roles,
            &project_roles,
            &explore,
            "devhome",
            "code",
        )
        .unwrap();
        assert_eq!(p.name, "devhome");
        // Explore never routes the supervisor either.
        let explore_supervisor: BTreeMap<String, String> =
            [("supervisor".to_string(), "devhome".to_string())].into();
        let s = resolve_provider(
            &providers,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &explore_supervisor,
            "",
            "supervisor",
        )
        .unwrap();
        assert_eq!(s.name, "anthropic");
    }

    #[test]
    fn resolve_provider_the_tasks_own_flag_wins_over_every_role() {
        let providers = providers(&["anthropic", "devhome", "openai"]);
        let operator_roles: BTreeMap<String, String> =
            [("code".to_string(), "devhome".to_string())].into();
        let project_roles: BTreeMap<String, String> =
            [("code".to_string(), "openai".to_string())].into();
        // The task flag sets every role, including one neither layer named.
        let p = resolve_provider(
            &providers,
            &operator_roles,
            &project_roles,
            &BTreeMap::new(),
            "devhome",
            "review",
        )
        .unwrap();
        assert_eq!(p.name, "devhome");
    }

    #[test]
    fn resolve_provider_an_unknown_name_is_refused_with_the_name_and_role() {
        let providers = providers(&["anthropic"]);
        let err = resolve_provider(
            &providers,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            "ghost",
            "plan",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ghost"), "{err}");
        assert!(err.contains("plan"), "{err}");
    }
}
