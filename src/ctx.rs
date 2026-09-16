//! The process-wide context: where things live, the store, the operator's
//! budget, the sandbox, and the reporter. Built once and shared.

use crate::agent;
use crate::config::{self, Budget};
use crate::report::Reporter;
use crate::sandbox::Sandbox;
use crate::store::Store;
use anyhow::{Context, Result};
use std::path::PathBuf;

pub struct Paths {
    pub home: PathBuf,
    pub worktrees: PathBuf,
    pub logs: PathBuf,
}

impl Paths {
    /// FORGE2_HOME, else $XDG_DATA_HOME/forge2, else ~/.local/share/forge2.
    /// Separate from Forge 1's FORGE_HOME so the two never share state.
    pub fn resolve() -> Result<Paths> {
        let home = if let Ok(p) = std::env::var("FORGE2_HOME") {
            PathBuf::from(p)
        } else if let Ok(p) = std::env::var("XDG_DATA_HOME") {
            PathBuf::from(p).join("forge2")
        } else {
            PathBuf::from(std::env::var("HOME").context("HOME is not set")?)
                .join(".local/share/forge2")
        };
        let p = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&p.worktrees)?;
        std::fs::create_dir_all(&p.logs)?;
        Ok(p)
    }
}

pub struct Forge {
    pub paths: Paths,
    pub store: Store,
    pub budget: Budget,
    pub supervisor: config::Supervisor,
    pub early_ending: config::EarlyEnding,
    pub measure: config::Measure,
    /// Agent backends by name, the built-in "anthropic" always present
    /// (see `config::load_home`).
    pub providers: std::collections::BTreeMap<String, agent::Provider>,
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
            // The repository map's parsed blobs are cached here and
            // written from inside the sandbox.
            let cache = paths.home.join("cache");
            let _ = std::fs::create_dir_all(&cache);
            Sandbox::detect(&agent::agent_bin(), &home.sandbox, extra_ro, vec![cache])?
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
            providers: home.providers,
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
            providers: home.providers,
            sandbox: None,
            report,
        })
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
