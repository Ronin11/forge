//! The process-wide context: where things live, the store, the operator's
//! budget, the sandbox, and the reporter. Built once and shared.

use crate::agent;
use crate::config::{self, Budget};
use crate::report::Reporter;
use crate::sandbox::Sandbox;
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
    pub sandbox: Option<Sandbox>,
    pub report: Reporter,
}

impl Forge {
    /// `need_agent` resolves the sandbox and agent binary, which only the
    /// commands that run attempts need. `prefix` tags output with task ids.
    pub fn open(need_agent: bool, prefix: bool) -> Result<Forge> {
        let paths = Paths::resolve()?;
        let store = Store::open(&paths.home.join("forge.db"))?;
        config::ensure_home_config(&paths.home)?;
        let home = config::load_home(&paths.home)?;
        let sandbox = if need_agent {
            // Forge's own tools (forge-repomap) live beside the binary.
            let mut extra_ro = Vec::new();
            if let Ok(exe) = std::env::current_exe()
                && let Some(dir) = exe.parent()
            {
                extra_ro.push(dir.to_path_buf());
            }
            Sandbox::detect(
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
            report,
        })
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
            report,
        })
    }

    /// Tell the sandbox what a repository's config lets attempts in
    /// `worktree` reach besides the model endpoint. No-op unsandboxed.
    pub fn allow_egress(&self, worktree: &Path, cfg: &config::Config, trust: crate::store::Trust) {
        if let Some(sandbox) = &self.sandbox {
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
        use crate::environment::Grant;
        let grant = self.environment.covers(need)?;
        let declared = matches!(
            self.trust_policy(trust).egress,
            config::TrustEgress::Declared
        );
        // Unsandboxed there is nothing to open, so the grant is recorded
        // and the run repeats all the same.
        let fresh = match (&self.sandbox, &grant) {
            (_, Grant::Host(_)) if !declared => false,
            (Some(sb), Grant::Host(h)) => {
                sb.grant_host(worktree, crate::egress::Rule::parse(h).ok()?)
            }
            (Some(sb), Grant::ReadOnly(p)) => sb.grant_ro(worktree, p.clone()),
            (None, _) => true,
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

    pub fn sandboxed(&self) -> bool {
        self.sandbox.is_some()
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

#[cfg(test)]
mod tests {
    use super::*;

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
