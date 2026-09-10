//! Two configs. `forge.toml` in the repository declares its checks; Forge
//! runs them and never trusts the agent's word for it, and reads them from
//! the trusted base commit so the branch under test cannot change what it
//! is verified against. `<FORGE2_HOME>/config.toml` is the operator's.

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
    check_timeout_secs: Option<u64>,
}

pub struct Config {
    pub checks: BTreeMap<String, Vec<String>>,
    pub base_branch: String,
    /// Remote to push succeeded branches to; `None` when the repo has no
    /// such remote or `push = false`.
    pub push_remote: Option<String>,
    pub check_timeout_secs: u64,
}

async fn parse(repo: &Path, text: &str, what: &str) -> Result<Config> {
    let raw: Raw = toml::from_str(text).with_context(|| format!("parsing {what}"))?;
    for (name, argv) in &raw.checks {
        if argv.is_empty() {
            bail!("check `{name}` has an empty command");
        }
    }
    let base_branch = match raw.defaults.base_branch {
        Some(b) => b,
        None => crate::git::current_branch(repo).await?,
    };
    let remote = raw.defaults.remote.unwrap_or_else(|| "origin".to_string());
    let push_remote = if raw.defaults.push.unwrap_or(true)
        && crate::git::remote_url(repo, &remote).await.is_some()
    {
        Some(remote)
    } else {
        None
    };
    Ok(Config {
        checks: raw.checks,
        base_branch,
        push_remote,
        check_timeout_secs: raw.defaults.check_timeout_secs.unwrap_or(600),
    })
}

/// The repository's forge.toml as it is in the working tree. Used when a
/// task is created, before any base commit is pinned.
pub async fn load_working(repo: &Path) -> Result<Config> {
    let path = repo.join("forge.toml");
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "{}: a repository must declare its checks in forge.toml",
            path.display()
        )
    })?;
    parse(repo, &text, &path.display().to_string()).await
}

/// The repository's forge.toml at `rev`: the trusted base for an attempt.
pub async fn load_at(repo: &Path, rev: &str) -> Result<Config> {
    let text = crate::git::show_file(repo, rev, "forge.toml")
        .await?
        .with_context(|| format!("forge.toml does not exist at {rev}"))?;
    parse(repo, &text, &format!("forge.toml at {rev}")).await
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

/// Operator-level caps. Both use the cost the claude CLI reports per
/// attempt; a running attempt is never killed by the budget.
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
