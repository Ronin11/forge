//! Two configs. `forge.toml` in the repository declares its checks; Forge
//! runs them and never trusts the agent's word for it, and reads them from
//! the trusted base commit so the branch under test cannot change what it
//! is verified against. `<FORGE2_HOME>/config.toml` is the operator's.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Default)]
struct Raw {
    #[serde(default)]
    checks: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    defaults: Defaults,
    #[serde(default)]
    verify: VerifyRaw,
}

#[derive(Deserialize, Default)]
struct VerifyRaw {
    /// Paths an attempt may not change unless the task allows it: the tests
    /// that guard the product, fixtures, CI config. A file, or a directory
    /// with a trailing slash.
    #[serde(default)]
    protected: Vec<String>,
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
    pub protected: Vec<String>,
}

/// Whether `path` falls under one of the protected entries.
pub fn is_protected(protected: &[String], path: &str) -> bool {
    protected.iter().any(|p| {
        if let Some(dir) = p.strip_suffix('/') {
            path.starts_with(dir) && path[dir.len()..].starts_with('/')
        } else {
            p == path
        }
    })
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
        protected: raw.verify.protected,
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
    #[serde(default)]
    sandbox: SandboxRaw,
}

#[derive(Deserialize, Default)]
struct SandboxRaw {
    ro_paths: Option<Vec<String>>,
    rw_paths: Option<Vec<String>>,
}

/// What the sandbox exposes beyond the attempt's own holes: toolchains the
/// checks need, read-only, and package caches, read-write and shared across
/// attempts. Paths that do not exist are skipped.
pub struct SandboxPaths {
    pub ro: Vec<PathBuf>,
    pub rw: Vec<PathBuf>,
}

pub struct HomeConfig {
    pub budget: Budget,
    pub sandbox: SandboxPaths,
}

fn expand(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
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

[sandbox]
# Read-only inside the sandbox: toolchains the checks need (node, cargo, ...).
# $HOME is otherwise empty in there, so anything installed under it goes here.
ro_paths = [\"~/.local/share/mise\"]
# Read-write inside the sandbox: package caches, shared across attempts. npm and
# cargo verify content against the lockfile, so a poisoned cache cannot change
# what installs.
rw_paths = [\"~/.npm\", \"~/.cargo/registry\", \"~/.cargo/git\"]
";

pub fn load_home(home: &Path) -> Result<HomeConfig> {
    let path = home.join("config.toml");
    if !path.exists() {
        std::fs::write(&path, DEFAULT_HOME_CONFIG)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let raw: HomeRaw =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let ro = raw
        .sandbox
        .ro_paths
        .unwrap_or_else(|| vec!["~/.local/share/mise".into()]);
    let rw = raw.sandbox.rw_paths.unwrap_or_else(|| {
        vec![
            "~/.npm".into(),
            "~/.cargo/registry".into(),
            "~/.cargo/git".into(),
        ]
    });
    Ok(HomeConfig {
        budget: Budget {
            per_task_usd: raw.budget.per_task_usd.unwrap_or(2.0),
            per_day_usd: raw.budget.per_day_usd.unwrap_or(20.0),
        },
        sandbox: SandboxPaths {
            ro: ro.iter().map(|p| expand(p)).collect(),
            rw: rw.iter().map(|p| expand(p)).collect(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_matches_files_and_directories() {
        let p = vec![
            "tests/progression.test.ts".to_string(),
            "fixtures/".to_string(),
        ];
        assert!(is_protected(&p, "tests/progression.test.ts"));
        assert!(!is_protected(&p, "tests/progression.test.ts.bak"));
        assert!(is_protected(&p, "fixtures/a.json"));
        assert!(!is_protected(&p, "fixtures2/a.json"));
        assert!(!is_protected(&p, "src/x.ts"));
    }

    #[test]
    fn home_config_defaults_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let c = load_home(dir.path()).unwrap();
        assert_eq!(c.budget.per_task_usd, 2.0);
        assert!(c.sandbox.rw.iter().any(|p| p.ends_with(".npm")));
        assert!(
            dir.path().join("config.toml").exists(),
            "defaults are written for the operator to edit"
        );
        std::fs::write(
            dir.path().join("config.toml"),
            "[sandbox]\nro_paths = [\"/opt/tools\"]\nrw_paths = []\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        assert_eq!(c.sandbox.ro, vec![PathBuf::from("/opt/tools")]);
        assert!(c.sandbox.rw.is_empty());
        assert_eq!(c.budget.per_day_usd, 20.0);
    }
}
