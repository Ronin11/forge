//! Two configs. `forge.toml` in the repository declares its checks; Forge
//! runs them and never trusts the agent's word for it, and reads them from
//! the trusted base commit so the branch under test cannot change what it
//! is verified against. `<FORGE2_HOME>/config.toml` is the operator's.

use crate::agent::{Provider, Runner};
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
    #[serde(default)]
    early_ending: EarlyEndingRaw,
    /// Extra roots to discover plugins under, beyond `<FORGE2_HOME>/plugins`
    /// (see `src/plugins.rs`). `~` expands; a relative path resolves against
    /// this config file's own directory (`<FORGE2_HOME>`).
    #[serde(default)]
    plugin_dirs: Vec<String>,
    #[serde(default)]
    measure: MeasureRaw,
    /// Agent backends beyond the built-in "anthropic" default; see
    /// `agent::Provider`.
    #[serde(default)]
    providers: BTreeMap<String, ProviderRaw>,
    /// Which provider each role runs under by default, overridable per
    /// project and per task (see `build_roles`).
    #[serde(default)]
    roles: RolesRaw,
}

#[derive(Deserialize, Default)]
struct ProviderRaw {
    runner: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    extra_args: Vec<String>,
    notes: Option<String>,
    price_usd_per_million_input: Option<f64>,
    price_usd_per_million_output: Option<f64>,
    /// This provider's own rate-window caps; default to `[budget]`'s when
    /// absent (see `build_providers`).
    five_hour_max: Option<f64>,
    seven_day_max: Option<f64>,
}

/// The five roles a provider is chosen for: the four contracts, and the
/// supervisor (which is not a contract but picks a provider the same way).
pub const ROLES: [&str; 5] = ["code", "tests", "review", "plan", "supervisor"];

#[derive(Deserialize, Default)]
struct RolesRaw {
    code: Option<String>,
    tests: Option<String>,
    review: Option<String>,
    plan: Option<String>,
    supervisor: Option<String>,
}

#[derive(Deserialize, Default)]
struct EarlyEndingRaw {
    no_edit_calls: Option<u32>,
    edits_without_commit: Option<u32>,
    repeats: Option<u32>,
    signals_to_end: Option<u32>,
}

/// Thresholds for `agent::Watch`, the live check that ends an attempt going
/// nowhere: no edit after this many tool calls, this many edits without a
/// commit, or one command run this many times. `signals_to_end` of these
/// tripping together ends the run; 0 disables early ending entirely.
#[derive(Clone, Copy, Debug)]
pub struct EarlyEnding {
    pub no_edit_calls: u32,
    pub edits_without_commit: u32,
    pub repeats: u32,
    pub signals_to_end: u32,
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
    pub early_ending: EarlyEnding,
    /// Extra plugin roots, in the order given, resolved to absolute paths.
    pub plugin_dirs: Vec<PathBuf>,
    pub measure: Measure,
    /// Agent backends by name, the built-in "anthropic" always present
    /// (overridable, but never absent) so a task naming no `--provider`
    /// always resolves to one.
    pub providers: BTreeMap<String, Provider>,
    /// Every role's default provider name (see `ROLES`); always has all
    /// five keys, "anthropic" where the operator named none.
    pub roles: BTreeMap<String, String>,
}

fn expand(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}

/// `~` expands; a relative path resolves against `config_dir` (the config
/// file's own directory), so what Forge discovers never depends on its
/// working directory.
fn resolve_config_relative(config_dir: &Path, p: &str) -> PathBuf {
    let expanded = expand(p);
    if expanded.is_relative() {
        config_dir.join(expanded)
    } else {
        expanded
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

#[derive(Deserialize, Default)]
struct MeasureRaw {
    journal_control: Option<f64>,
}

/// Fixed fractions of tasks the operator assigns to a control arm so a
/// measurement accumulates on its own, without touching every task by
/// hand (see docs/LATER.md, "The journal measurement was ill-posed three
/// times").
pub struct Measure {
    /// Fraction of tasks, chosen deterministically from the task id, that
    /// run with the journal off when the request itself does not say
    /// `--journal` or `--no-journal`. `0.0` (the default) assigns none.
    pub journal_control: f64,
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

[early_ending]
# The live check that ends an attempt when it looks like it is going
# nowhere: no edit after this many tool calls, this many edits without a
# commit, or one command run this many times. `signals_to_end` of these
# tripping together ends the run early (the session is kept and resumed
# with a prompt naming them); 0 disables early ending entirely.
no_edit_calls = 30
edits_without_commit = 15
repeats = 5
signals_to_end = 2

[measure]
# Fixed fraction of tasks assigned to the journal's control arm (run with
# no journal), chosen deterministically from the task id, when a task's
# own request does not say --journal or --no-journal. 0.0 assigns none;
# see docs/LATER.md, \"The journal measurement was ill-posed three times\".
journal_control = 0.0

# Agent backends beyond the built-in \"anthropic\" provider (runner
# claude-cli, today's models; no entry needed to keep today's behavior). A
# task picks one with `forge add --provider <name>`; `forge providers`
# lists what is configured. `env` and `extra_args` are the runner's own
# process env and argv; `price_usd_per_million_input/output` price a
# runner that reports no cost itself (codex), 0 for a local model.
#
# [providers.devhome]
# runner = \"codex-cli\"
# model = \"qwen3-coder:30b\"
# env = { CODEX_OSS_BASE_URL = \"http://dev.home:11434/v1\", OLLAMA_HOST = \"http://dev.home:11434\" }
# extra_args = [\"--oss\", \"--local-provider\", \"ollama\"]
#
# [providers.openai]
# runner = \"codex-cli\"
# notes = \"signed in with codex login\"

# Which provider each role runs under by default: the four contracts
# (code, tests, review, plan) and the supervisor. \"anthropic\" where a
# role names none. A project can override a role with `forge project set
# --role <role>=<provider>`; a task's own `--provider` wins over both and
# sets every role for that task.
#
# [roles]
# review = \"devhome\"
";

/// Write the operator's config the first time `home` is used, so there is a
/// file for them to edit; never overwrites one that already exists. Called
/// once, from `Forge::open`: reading the config (`load_home`) must never
/// have the side effect of writing it, or every read-only verb would too.
pub fn ensure_home_config(home: &Path) -> Result<()> {
    let path = home.join("config.toml");
    if !path.exists() {
        std::fs::write(&path, DEFAULT_HOME_CONFIG)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

pub fn load_home(home: &Path) -> Result<HomeConfig> {
    let path = home.join("config.toml");
    let raw: HomeRaw = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HomeRaw::default(),
        Err(e) => return Err(e).context(format!("reading {}", path.display())),
    };
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
    let budget = Budget {
        per_task_usd: raw.budget.per_task_usd.unwrap_or(2.0),
        per_day_usd: raw.budget.per_day_usd,
        five_hour_max: raw.budget.five_hour_max.unwrap_or(0.9),
        seven_day_max: raw.budget.seven_day_max.unwrap_or(0.95),
    };
    let providers = build_providers(raw.providers, &budget)?;
    let roles = build_roles(raw.roles, &providers)?;
    Ok(HomeConfig {
        budget,
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
        early_ending: EarlyEnding {
            no_edit_calls: raw.early_ending.no_edit_calls.unwrap_or(30),
            edits_without_commit: raw.early_ending.edits_without_commit.unwrap_or(15),
            repeats: raw.early_ending.repeats.unwrap_or(5),
            signals_to_end: raw.early_ending.signals_to_end.unwrap_or(2),
        },
        plugin_dirs: raw
            .plugin_dirs
            .iter()
            .map(|p| resolve_config_relative(home, p))
            .collect(),
        measure: Measure {
            journal_control: raw.measure.journal_control.unwrap_or(0.0),
        },
        providers,
        roles,
    })
}

/// The built-in "anthropic" provider, plus every `[providers.<name>]` table
/// the operator declared; a table named "anthropic" overrides the built-in
/// rather than duplicating it, so an operator can, say, give it its own
/// price table without losing the runner and model every existing config
/// already relies on.
fn build_providers(
    raw: BTreeMap<String, ProviderRaw>,
    budget: &Budget,
) -> Result<BTreeMap<String, Provider>> {
    let mut providers = BTreeMap::new();
    providers.insert(
        "anthropic".to_string(),
        Provider {
            five_hour_max: budget.five_hour_max,
            seven_day_max: budget.seven_day_max,
            ..Provider::default()
        },
    );
    for (name, p) in raw {
        let runner = match &p.runner {
            Some(r) => r
                .parse::<Runner>()
                .map_err(|e| anyhow::anyhow!("providers.{name}: {e}"))?,
            None if name == "anthropic" => Runner::ClaudeCli,
            None => bail!("providers.{name}: needs a `runner`"),
        };
        providers.insert(
            name.clone(),
            Provider {
                name,
                runner,
                model: p.model,
                base_url: p.base_url,
                env: p.env.into_iter().collect(),
                extra_args: p.extra_args,
                notes: p.notes,
                price_input_per_million: p.price_usd_per_million_input.unwrap_or(0.0),
                price_output_per_million: p.price_usd_per_million_output.unwrap_or(0.0),
                five_hour_max: p.five_hour_max.unwrap_or(budget.five_hour_max),
                seven_day_max: p.seven_day_max.unwrap_or(budget.seven_day_max),
            },
        );
    }
    Ok(providers)
}

/// Every role's default provider (see `ROLES`): the operator's `[roles]`
/// table, "anthropic" where it names none. Each name must be a configured
/// provider, checked here so a typo fails at startup, not mid-task.
fn build_roles(
    raw: RolesRaw,
    providers: &BTreeMap<String, Provider>,
) -> Result<BTreeMap<String, String>> {
    let mut roles = BTreeMap::new();
    for (role, v) in [
        ("code", raw.code),
        ("tests", raw.tests),
        ("review", raw.review),
        ("plan", raw.plan),
        ("supervisor", raw.supervisor),
    ] {
        let name = v.unwrap_or_else(|| "anthropic".to_string());
        if !providers.contains_key(&name) {
            bail!(
                "roles.{role}: unknown provider {name:?}; see `forge providers` for what is configured"
            );
        }
        roles.insert(role.to_string(), name);
    }
    Ok(roles)
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
            !dir.path().join("config.toml").exists(),
            "reading the config must not write it"
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

    #[test]
    fn ensure_home_config_writes_defaults_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        ensure_home_config(dir.path()).unwrap();
        assert!(
            path.exists(),
            "defaults are written for the operator to edit"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_HOME_CONFIG);
        std::fs::write(&path, "[budget]\nper_task_usd = 9.0\n").unwrap();
        ensure_home_config(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[budget]\nper_task_usd = 9.0\n",
            "an existing config is never overwritten"
        );
    }

    #[test]
    fn plugin_dirs_expand_tilde_and_resolve_relative_paths_against_home() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "plugin_dirs = [\"~/.config/forge2/plugins\", \"relative/plugins\", \"/opt/forge2/plugins\"]\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        assert_eq!(
            c.plugin_dirs,
            vec![
                PathBuf::from(std::env::var("HOME").unwrap()).join(".config/forge2/plugins"),
                dir.path().join("relative/plugins"),
                PathBuf::from("/opt/forge2/plugins"),
            ]
        );
    }

    /// The template written for a fresh operator and `load_home`'s own
    /// fallbacks must agree: the defaults exist in two places (the template
    /// text and the `unwrap_or` calls), and drift between them would leave
    /// a freshly written config.toml describing values the code does not
    /// actually fall back to.
    #[test]
    fn default_home_config_matches_load_homes_fallback_defaults() {
        let templated = tempfile::tempdir().unwrap();
        std::fs::write(templated.path().join("config.toml"), DEFAULT_HOME_CONFIG).unwrap();
        let from_template = load_home(templated.path()).unwrap();

        let empty = tempfile::tempdir().unwrap();
        let from_defaults = load_home(empty.path()).unwrap();

        assert_eq!(
            from_template.budget.per_task_usd,
            from_defaults.budget.per_task_usd
        );
        assert_eq!(
            from_template.budget.per_day_usd,
            from_defaults.budget.per_day_usd
        );
        assert_eq!(
            from_template.budget.five_hour_max,
            from_defaults.budget.five_hour_max
        );
        assert_eq!(
            from_template.budget.seven_day_max,
            from_defaults.budget.seven_day_max
        );
        assert_eq!(from_template.sandbox.ro, from_defaults.sandbox.ro);
        assert_eq!(from_template.sandbox.rw, from_defaults.sandbox.rw);
        assert_eq!(
            from_template.supervisor.enabled,
            from_defaults.supervisor.enabled
        );
        assert_eq!(
            from_template.supervisor.model,
            from_defaults.supervisor.model
        );
        assert_eq!(
            from_template.supervisor.max_turns,
            from_defaults.supervisor.max_turns
        );
        assert_eq!(
            from_template.supervisor.timeout_secs,
            from_defaults.supervisor.timeout_secs
        );
        assert_eq!(
            from_template.supervisor.per_lineage,
            from_defaults.supervisor.per_lineage
        );
        assert_eq!(
            from_template.early_ending.no_edit_calls,
            from_defaults.early_ending.no_edit_calls
        );
        assert_eq!(
            from_template.early_ending.edits_without_commit,
            from_defaults.early_ending.edits_without_commit
        );
        assert_eq!(
            from_template.early_ending.repeats,
            from_defaults.early_ending.repeats
        );
        assert_eq!(
            from_template.early_ending.signals_to_end,
            from_defaults.early_ending.signals_to_end
        );
    }

    #[test]
    fn early_ending_overrides_from_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[early_ending]\nno_edit_calls = 10\nedits_without_commit = 4\nrepeats = 3\nsignals_to_end = 0\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        assert_eq!(c.early_ending.no_edit_calls, 10);
        assert_eq!(c.early_ending.edits_without_commit, 4);
        assert_eq!(c.early_ending.repeats, 3);
        assert_eq!(c.early_ending.signals_to_end, 0);
    }

    #[test]
    fn the_anthropic_provider_is_built_in() {
        let dir = tempfile::tempdir().unwrap();
        let c = load_home(dir.path()).unwrap();
        let p = &c.providers["anthropic"];
        assert_eq!(p.runner, crate::agent::Runner::ClaudeCli);
        assert_eq!(p.model.as_deref(), Some("sonnet"));
    }

    /// The two commented examples in `DEFAULT_HOME_CONFIG`, uncommented:
    /// they must parse into the fields the task said they carry.
    #[test]
    fn provider_tables_parse_runner_model_env_and_extra_args() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[providers.devhome]\n\
             runner = \"codex-cli\"\n\
             model = \"qwen3-coder:30b\"\n\
             env = { CODEX_OSS_BASE_URL = \"http://dev.home:11434/v1\" }\n\
             extra_args = [\"--oss\", \"--local-provider\", \"ollama\"]\n\
             \n\
             [providers.openai]\n\
             runner = \"codex-cli\"\n\
             notes = \"signed in with codex login\"\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        let devhome = &c.providers["devhome"];
        assert_eq!(devhome.runner, crate::agent::Runner::CodexCli);
        assert_eq!(devhome.model.as_deref(), Some("qwen3-coder:30b"));
        assert_eq!(
            devhome.env,
            vec![(
                "CODEX_OSS_BASE_URL".to_string(),
                "http://dev.home:11434/v1".to_string()
            )]
        );
        assert_eq!(
            devhome.extra_args,
            vec!["--oss", "--local-provider", "ollama"]
        );
        assert_eq!(devhome.price_input_per_million, 0.0);

        let openai = &c.providers["openai"];
        assert_eq!(openai.runner, crate::agent::Runner::CodexCli);
        assert_eq!(openai.model, None);
        assert!(openai.env.is_empty());
        assert_eq!(openai.notes.as_deref(), Some("signed in with codex login"));

        // The built-in default is still there alongside the operator's own.
        assert_eq!(
            c.providers["anthropic"].runner,
            crate::agent::Runner::ClaudeCli
        );
    }

    #[test]
    fn roles_default_to_anthropic_and_provider_caps_default_to_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let c = load_home(dir.path()).unwrap();
        for role in ROLES {
            assert_eq!(c.roles[role], "anthropic", "role {role}");
        }
        assert_eq!(c.providers["anthropic"].five_hour_max, 0.9);
        assert_eq!(c.providers["anthropic"].seven_day_max, 0.95);
    }

    #[test]
    fn roles_can_be_overridden_per_role_and_providers_can_override_their_own_caps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[budget]\nfive_hour_max = 0.8\n\
             [providers.devhome]\n\
             runner = \"codex-cli\"\n\
             five_hour_max = 0.5\n\
             \n\
             [roles]\n\
             code = \"devhome\"\n\
             review = \"devhome\"\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        assert_eq!(c.roles["code"], "devhome");
        assert_eq!(c.roles["review"], "devhome");
        assert_eq!(c.roles["tests"], "anthropic");
        assert_eq!(c.roles["plan"], "anthropic");
        assert_eq!(c.roles["supervisor"], "anthropic");
        // Overridden explicitly.
        assert_eq!(c.providers["devhome"].five_hour_max, 0.5);
        // Not overridden: falls to the operator's own budget cap.
        assert_eq!(c.providers["devhome"].seven_day_max, 0.95);
        assert_eq!(c.providers["anthropic"].five_hour_max, 0.8);
    }

    #[test]
    fn a_role_naming_an_unconfigured_provider_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[roles]\ncode = \"does-not-exist\"\n",
        )
        .unwrap();
        let err = match load_home(dir.path()) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("does-not-exist"), "{err}");
    }

    #[test]
    fn an_unknown_runner_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[providers.bogus]\nrunner = \"not-a-runner\"\n",
        )
        .unwrap();
        let err = match load_home(dir.path()) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("not-a-runner"), "{err}");
    }
}
