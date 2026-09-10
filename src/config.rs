//! `forge.toml`: the repository declares its checks; Forge runs them and
//! never trusts the agent's word for it.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Deserialize, Default)]
struct Raw {
    #[serde(default)]
    checks: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    defaults: Defaults,
}

#[derive(Deserialize, Default)]
struct Defaults {
    base_branch: Option<String>,
    remote: Option<String>,
    push: Option<bool>,
}

pub struct Config {
    pub checks: BTreeMap<String, Vec<String>>,
    pub base_branch: String,
    /// Remote to push succeeded branches to; `None` when the repo has no
    /// such remote or `push = false`.
    pub push_remote: Option<String>,
}

pub fn load(repo: &Path) -> Result<Config> {
    let path = repo.join("forge.toml");
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "{}: a repository must declare its checks in forge.toml",
            path.display()
        )
    })?;
    let raw: Raw = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    for (name, argv) in &raw.checks {
        if argv.is_empty() {
            bail!("check `{name}` has an empty command");
        }
    }
    let base_branch = match raw.defaults.base_branch {
        Some(b) => b,
        None => crate::git::current_branch(repo)?,
    };
    let remote = raw.defaults.remote.unwrap_or_else(|| "origin".to_string());
    let push_remote =
        if raw.defaults.push.unwrap_or(true) && crate::git::remote_url(repo, &remote).is_some() {
            Some(remote)
        } else {
            None
        };
    Ok(Config {
        checks: raw.checks,
        base_branch,
        push_remote,
    })
}

#[derive(Deserialize, Default)]
struct HomeRaw {
    #[serde(default)]
    budget: BudgetRaw,
}

#[derive(Deserialize, Default)]
struct BudgetRaw {
    per_task_usd: Option<f64>,
    per_day_usd: Option<f64>,
}

/// Operator-level caps, from `<FORGE2_HOME>/config.toml`. Both use the
/// cost the claude CLI reports per attempt; a running attempt is never
/// killed by the budget, its --max-turns is the cliff.
pub struct Budget {
    pub per_task_usd: f64,
    pub per_day_usd: f64,
}

const DEFAULT_HOME_CONFIG: &str = "\
# Forge 2 operator config. Budgets use the cost the claude CLI reports per attempt.
[budget]
per_task_usd = 2.0    # a task stops retrying once its attempts have cost this much
per_day_usd = 20.0    # no new task is claimed once the last 24 hours cost this much
";

pub fn load_budget(home: &Path) -> Result<Budget> {
    let path = home.join("config.toml");
    if !path.exists() {
        std::fs::write(&path, DEFAULT_HOME_CONFIG)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let raw: HomeRaw =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Budget {
        per_task_usd: raw.budget.per_task_usd.unwrap_or(2.0),
        per_day_usd: raw.budget.per_day_usd.unwrap_or(20.0),
    })
}
