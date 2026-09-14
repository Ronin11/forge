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
    /// Directories owned by verification: hidden from the coder, overlaid
    /// from the trusted refs at verify time. Must not exist in the base tree.
    #[serde(default)]
    namespace: Vec<String>,
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
    pub namespace: Vec<String>,
    /// Where the repository's config actually lives: `forge.toml` or
    /// `.forge/forge.toml`. Whatever this is, it is the path every rule
    /// that used to say `forge.toml` by name now means.
    pub config_path: String,
}

/// The repository's config location: `.forge/forge.toml` if it exists,
/// else `forge.toml` at the root. Both existing is an error naming both.
const ALT_CONFIG_PATH: &str = ".forge/forge.toml";
const ROOT_CONFIG_PATH: &str = "forge.toml";

/// Whether `path` is inside a write scope: a file, a directory with a
/// trailing slash, or a `*.ext` suffix pattern.
pub fn in_scope(scope: &[String], path: &str) -> bool {
    scope.iter().any(|p| {
        if let Some(suffix) = p.strip_prefix('*') {
            path.ends_with(suffix)
        } else if let Some(dir) = p.strip_suffix('/') {
            path.starts_with(dir) && path[dir.len()..].starts_with('/')
        } else {
            p == path
        }
    })
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

async fn parse(repo: &Path, text: &str, what: &str, config_path: &str) -> Result<Config> {
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
        // `forge.toml` by name is the long-standing convention for
        // protecting the repository's own config; when the config actually
        // lives elsewhere, that convention must protect the real path.
        protected: raw
            .verify
            .protected
            .into_iter()
            .map(|p| {
                if p == ROOT_CONFIG_PATH {
                    config_path.to_string()
                } else {
                    p
                }
            })
            .collect(),
        namespace: raw
            .verify
            .namespace
            .into_iter()
            .map(|d| if d.ends_with('/') { d } else { format!("{d}/") })
            .collect(),
        config_path: config_path.to_string(),
    })
}

/// The repository's config as it is in the working tree: `.forge/forge.toml`
/// if it exists, else `forge.toml` at the root. Both existing is an error.
/// Used when a task is created, before any base commit is pinned.
pub async fn load_working(repo: &Path) -> Result<Config> {
    let alt = repo.join(ALT_CONFIG_PATH);
    let root = repo.join(ROOT_CONFIG_PATH);
    let (path, config_path, text) = match (alt.exists(), root.exists()) {
        (true, true) => bail!(
            "both {} and {} exist; a repository must declare its checks in only one",
            alt.display(),
            root.display()
        ),
        (true, false) => {
            let text = std::fs::read_to_string(&alt)
                .with_context(|| format!("reading {}", alt.display()))?;
            (alt, ALT_CONFIG_PATH, text)
        }
        (false, _) => {
            let text = std::fs::read_to_string(&root).with_context(|| {
                format!(
                    "{}: a repository must declare its checks in forge.toml",
                    root.display()
                )
            })?;
            (root, ROOT_CONFIG_PATH, text)
        }
    };
    parse(repo, &text, &path.display().to_string(), config_path).await
}

/// The repository's config at `rev`: the trusted base for an attempt.
/// `.forge/forge.toml` if it exists there, else `forge.toml` at the root.
/// `repo` answers questions about remotes; `show_dir` is where `rev` is read
/// from (the task's clone, which has the same objects).
pub async fn load_at(repo: &Path, show_dir: &Path, rev: &str) -> Result<Config> {
    let alt = crate::git::show_file(show_dir, rev, ALT_CONFIG_PATH).await?;
    let root = crate::git::show_file(show_dir, rev, ROOT_CONFIG_PATH).await?;
    let (config_path, text) = match (alt, root) {
        (Some(_), Some(_)) => bail!(
            "both {ALT_CONFIG_PATH} and {ROOT_CONFIG_PATH} exist at {rev}; a repository must declare its checks in only one"
        ),
        (Some(t), None) => (ALT_CONFIG_PATH, t),
        (None, Some(t)) => (ROOT_CONFIG_PATH, t),
        (None, None) => bail!("forge.toml does not exist at {rev}"),
    };
    parse(repo, &text, &format!("{config_path} at {rev}"), config_path).await
}

#[derive(Deserialize, Default)]
struct HomeRaw {
    #[serde(default)]
    budget: BudgetRaw,
    #[serde(default)]
    sandbox: SandboxRaw,
    #[serde(default)]
    supervisor: SupervisorRaw,
}

#[derive(Deserialize, Default)]
struct SupervisorRaw {
    enabled: Option<bool>,
    model: Option<String>,
    max_turns: Option<u32>,
    timeout_secs: Option<u64>,
    per_lineage: Option<u32>,
}

/// The repository supervisor: the rung between a blocked task and the
/// human. A read-only agent on a strong model that answers a question
/// with citations, files a prerequisite task, or escalates.
#[derive(Clone, Debug)]
pub struct Supervisor {
    pub enabled: bool,
    pub model: String,
    pub max_turns: u32,
    pub timeout_secs: u64,
    /// How many times the supervisor may answer within one piece of work
    /// before the question goes to the human regardless.
    pub per_lineage: u32,
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
    pub supervisor: Supervisor,
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
    five_hour_max: Option<f64>,
    seven_day_max: Option<f64>,
}

/// Operator-level caps. The dollar caps use the cost the claude CLI
/// reports per attempt; a running attempt is never killed by them. The
/// window caps are fractions of the subscription's rate windows as the
/// CLI reports them: at or above a cap the worker holds until the window
/// resets, then continues.
pub struct Budget {
    /// A task stops retrying once its attempts have cost this much: the
    /// runaway guard.
    pub per_task_usd: f64,
    /// No new task is claimed once the last 24 hours cost this much;
    /// `None` is no cap, which is right for a subscription.
    pub per_day_usd: Option<f64>,
    pub five_hour_max: f64,
    pub seven_day_max: f64,
}

const DEFAULT_HOME_CONFIG: &str = "\
# Forge 2 operator config.
[budget]
# The subscription's rate windows, as fractions of each window the claude CLI
# reports after every attempt. At or above a cap the worker holds until the
# window resets, then continues; nothing fails because of it.
five_hour_max = 0.9
seven_day_max = 0.95
# A task stops retrying once its attempts have cost this much (the CLI's own
# accounting): the runaway guard.
per_task_usd = 2.0
# Optional: no new task is claimed once the last 24 hours cost this much.
# Leave it out on a subscription; the windows above are the real limit.
# per_day_usd = 20.0

[sandbox]
# Read-only inside the sandbox: toolchains the checks need (node, cargo, ...).
# $HOME is otherwise empty in there, so anything installed under it goes here.
ro_paths = [\"~/.local/share/mise\"]
# Read-write inside the sandbox: package caches, shared across attempts. npm and
# cargo verify content against the lockfile, so a poisoned cache cannot change
# what installs.
rw_paths = [\"~/.npm\", \"~/.cargo/registry\", \"~/.cargo/git\"]

[supervisor]
# When a task blocks with a question, a read-only agent on a strong model
# reads the repository's record and answers with citations, files a
# prerequisite task, or escalates to you. Off: every question is yours.
enabled = true
model = \"opus\"
max_turns = 30
# Answers per piece of work before the question reaches you regardless.
per_lineage = 2
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
            per_day_usd: raw.budget.per_day_usd,
            five_hour_max: raw.budget.five_hour_max.unwrap_or(0.9),
            seven_day_max: raw.budget.seven_day_max.unwrap_or(0.95),
        },
        sandbox: SandboxPaths {
            ro: ro.iter().map(|p| expand(p)).collect(),
            rw: rw.iter().map(|p| expand(p)).collect(),
        },
        supervisor: Supervisor {
            // FORGE2_SUPERVISOR=0 turns it off for one process: the e2e
            // suite's default, and an operator's quick switch.
            enabled: raw.supervisor.enabled.unwrap_or(true)
                && std::env::var("FORGE2_SUPERVISOR")
                    .map(|v| v != "0")
                    .unwrap_or(true),
            model: raw.supervisor.model.unwrap_or_else(|| "opus".into()),
            max_turns: raw.supervisor.max_turns.unwrap_or(30),
            timeout_secs: raw.supervisor.timeout_secs.unwrap_or(900),
            per_lineage: raw.supervisor.per_lineage.unwrap_or(2),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_matches_dirs_files_and_suffixes() {
        let sc = vec![
            "docs/".to_string(),
            "*.md".to_string(),
            "CHANGELOG".to_string(),
        ];
        assert!(in_scope(&sc, "docs/a/b.txt"));
        assert!(in_scope(&sc, "README.md"));
        assert!(in_scope(&sc, "src/deep/notes.md"));
        assert!(in_scope(&sc, "CHANGELOG"));
        assert!(!in_scope(&sc, "src/main.rs"));
        assert!(!in_scope(&sc, "docsx/a"));
    }

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

    #[tokio::test]
    async fn both_config_locations_present_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir(repo.join(".forge")).unwrap();
        std::fs::write(repo.join(".forge/forge.toml"), "[checks]\n").unwrap();
        std::fs::write(repo.join("forge.toml"), "[checks]\n").unwrap();
        let err = match load_working(repo).await {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains(".forge/forge.toml"), "{err}");
        assert!(err.contains("forge.toml"), "{err}");
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
        assert_eq!(c.budget.per_day_usd, None);
        assert_eq!(c.budget.five_hour_max, 0.9);
    }
}
