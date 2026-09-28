//! `<FORGE_HOME>/config.toml`: the operator's own config, distinct from a
//! repository's `forge.toml` (see the crate doc comment). Read once per
//! process by `ctx::Forge::open`; a fresh operator gets `DEFAULT_HOME_CONFIG`
//! written for them to edit (`ensure_home_config`).

use super::measure::{Measure, MeasureRaw, build_explore};
use super::providers::{ProviderRaw, RolesRaw, build_providers, build_roles};
use super::trust::{TrustPolicies, TrustRaw, build_trust};
use crate::agent::Provider;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    /// Extra roots to discover plugins under, beyond `<FORGE_HOME>/plugins`
    /// (see `src/plugins.rs`). `~` expands; a relative path resolves against
    /// this config file's own directory (`<FORGE_HOME>`).
    #[serde(default)]
    plugin_dirs: Vec<String>,
    #[serde(default)]
    measure: MeasureRaw,
    #[serde(default)]
    intake: IntakeRaw,
    /// `[trust.<level>]`: the policy each trust level a task can carry is
    /// judged against at enqueue (see `build_trust`,
    /// `queue::apply_trust_policy`).
    #[serde(default)]
    trust: TrustRaw,
    /// Agent backends beyond the built-in "anthropic" default; see
    /// `agent::Provider`.
    #[serde(default)]
    providers: BTreeMap<String, ProviderRaw>,
    /// Which provider each role runs under by default, overridable per
    /// project and per task (see `build_roles`).
    #[serde(default)]
    roles: RolesRaw,
    /// Per-project settings kept in the operator's own config rather than
    /// the store: today just a project's secrets (see `ProjectHomeRaw`).
    #[serde(default)]
    projects: BTreeMap<String, ProjectHomeRaw>,
    /// `[secrets]`: name to environment variable the worker already has
    /// (see `crate::secrets`), never a value.
    #[serde(default)]
    secrets: BTreeMap<String, crate::secrets::Entry>,
    /// `[environment]`: what the kernel grants a repository automatically
    /// when an attempt fails on a missing tool (see `environment`).
    #[serde(default)]
    environment: EnvironmentRaw,
}

/// `[environment]` in the operator's config; an absent key keeps its default.
#[derive(Deserialize, Default)]
struct EnvironmentRaw {
    hosts: Option<Vec<String>>,
    cache_paths: Option<Vec<String>>,
}

/// `[projects.<name>]` in the operator's config.
#[derive(Deserialize, Default)]
struct ProjectHomeRaw {
    /// Injected as environment for that project's jobs (`forge job start`),
    /// never into a prompt (see docs/JOBS.md, "Security posture").
    #[serde(default)]
    secrets: BTreeMap<String, String>,
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
    dependency_cache: Option<String>,
}

/// What the sandbox exposes beyond the attempt's own holes: toolchains the
/// checks need, read-only, and package caches, read through with an
/// attempt's own writes going to a private overlay discarded with it (see
/// `sandbox::Sandbox::command`), so one attempt can never poison what
/// another reads from these. Paths that do not exist are skipped.
pub struct SandboxPaths {
    pub ro: Vec<PathBuf>,
    pub rw: Vec<PathBuf>,
    /// `[sandbox] dependency_cache`: a directory the operator warms with
    /// the repository's dependencies, bound read-only into every attempt so
    /// a task at a `model`-egress trust level can run its checks with no
    /// registry reachable. `None` when not configured.
    pub dependency_cache: Option<PathBuf>,
}

pub struct HomeConfig {
    pub budget: Budget,
    pub sandbox: SandboxPaths,
    pub supervisor: Supervisor,
    pub early_ending: EarlyEnding,
    /// Extra plugin roots, in the order given, resolved to absolute paths.
    pub plugin_dirs: Vec<PathBuf>,
    pub measure: Measure,
    pub intake: Intake,
    /// `[trust.<level>]`: the policy per trust level, defaulted per
    /// `build_trust`; enforced at enqueue (`queue::apply_trust_policy`).
    pub trust: TrustPolicies,
    /// Agent backends by name, the built-in "anthropic" always present
    /// (overridable, but never absent) so a task naming no `--provider`
    /// always resolves to one.
    pub providers: BTreeMap<String, Provider>,
    /// Every role's default provider name (see `ROLES`); always has all
    /// five keys, "anthropic" where the operator named none.
    pub roles: BTreeMap<String, String>,
    /// A project's secrets, by project name (see `ProjectHomeRaw`); a
    /// project the operator declared none for is absent, not empty.
    pub project_secrets: BTreeMap<String, BTreeMap<String, String>>,
    /// `[secrets]`: name to environment variable (`crate::secrets`).
    pub secrets: BTreeMap<String, String>,
    /// `[environment]`: the hosts and host cache paths granted automatically.
    pub environment: crate::environment::Policy,
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
struct IntakeRaw {
    max_questions_per_day: Option<u32>,
}

/// The `interview` directive's own cap, separate from `[budget]`: how
/// many questions it may ask a person in a rolling 24 hours before the
/// worker leaves its tasks queued rather than start a turn that would
/// ask another (see docs/INTAKE.md, "A per-interview budget and a cap on
/// questions per day").
#[derive(Clone, Copy, Debug)]
pub struct Intake {
    pub max_questions_per_day: u32,
}

const DEFAULT_HOME_CONFIG: &str = "\
# Forge operator config.
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
# Optional: a directory you warm with the repository's dependencies, bound
# read-only into every attempt. A task at a trust level whose egress is
# \"model\" reaches no registry, so its checks install from here.
# dependency_cache = \"~/.cache/forge-deps\"

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

# What an hour of your attention is worth, in dollars, and how many minutes
# answering one question takes you: `forge stats --questions` multiplies the
# questions a person handled by both. Leave the rate out to price nothing.
# operator_usd_per_hour = 100.0
# attention_minutes_per_question = 5.0

# Per-role exploration: send a fixed fraction of a role's steps to a named
# provider instead of its usual one, chosen deterministically from the task
# id, when the task itself names no --provider (an explicit --provider is
# never overridden). `forge stats --by-role` already splits by provider, so
# this is how head-to-head data on a role accumulates without routing tasks
# to it by hand.
#
# [measure.explore.code]
# provider = \"devhome\"
# fraction = 0.1

# Agent backends beyond the built-in \"anthropic\" provider (runner
# claude-cli, today's models; no entry needed to keep today's behavior). A
# task picks one with `forge add --provider <name>`; `forge providers`
# lists what is configured. `env` and `extra_args` are the runner's own
# process env and argv; `price_usd_per_million_input/output` price a
# runner that reports no cost itself (codex), 0 for a local model. A
# claude-cli provider may name a `model` too (an opus arm for the
# economist): it wins over the task's default, but not over `--model`. On a
# claude-cli provider they price every attempt at the API list from the
# CLI's tokens (cache reads at `price_usd_per_million_cache_read`, default a
# tenth of the input price; cache creation at the input price) and keep the
# CLI's own figure in `cli_cost_usd`. List prices as of 2026-09-25, USD per
# million tokens: Opus 5.5 $4 in / $20 out, Sonnet 5 $2 in / $10 out.
#
# [providers.anthropic]
# price_usd_per_million_input = 2.00
# price_usd_per_million_output = 10.00
# price_usd_per_million_cache_read = 0.20
#
# [providers.devhome]
# runner = \"codex-cli\"
# model = \"qwen3-coder:30b\"
# env = { CODEX_OSS_BASE_URL = \"http://dev.home:11434/v1\", OLLAMA_HOST = \"http://dev.home:11434\" }
# extra_args = [\"--oss\", \"--local-provider\", \"ollama\", \"-c\", \"include_apply_patch_tool=true\"]
# (the apply_patch tool is off for models codex does not know; without it a
# local model can read but not edit)
# nudges = 3
# (a weak model often ends codex's phase one having only read files, or
# having edited without committing; up to this many times, run_codex
# resumes the same thread with a fixed prompt to do the work and commit
# before phase two ever asks for the structured report)
#
# [providers.openai]
# runner = \"codex-cli\"
# notes = \"signed in with codex login\"
#
# [providers.copilot]
# runner = \"copilot-cli\"
# notes = \"signed in with copilot login; a plan's premium requests\"
# price_usd_per_premium_request = 0.04
# (copilot meters premium requests, not tokens: 0 while the plan's monthly
# allowance lasts, the list price per request over it. `api_key_env` may
# name a variable holding a GitHub token, passed to the CLI as
# COPILOT_GITHUB_TOKEN ahead of its stored login; the value never lives in
# this file.)

# runner = \"chat\" spawns no agent CLI at all: one HTTP call to an
# OpenAI-compatible /chat/completions endpoint. It is refused for anything
# but a job's directive step (docs/JOBS.md, \"Steps\"), which has no tools
# by design and so needs no agent to act with — a task's code step still
# runs through claude-cli or codex-cli. `base_url` is required;
# `api_key_env` names the environment variable holding the key (never the
# key itself, which never lives in this file), absent for an endpoint that
# needs none (a local model).
#
# [providers.devhome-chat]
# runner = \"chat\"
# base_url = \"http://dev.home:11434/v1\"
# model = \"qwen3-coder:30b\"
#
# runner = \"jev\" is TypeSafe's typed judgment of a directive step's outcomes
# (docs/EXECUTION.md, \"The judgment tier\"). Every key below is its default;
# `*_env` keys name variables. `auto` asks Cloudflare first until its 402.
#
# [providers.jev]
# runner = \"jev\"
# backend = \"auto\"
# base_url = \"https://api.typesafe.ai/v1/systemone\"
# api_key_env = \"TYPESAFE_API_KEY\"
# model = \"jev-latest\"
# price_usd_per_million_input = 0.042
# cloudflare_url = \"https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/run\"
# account_id_env = \"CLOUDFLARE_ACCOUNT_ID\"
# cloudflare_api_key_env = \"CLOUDFLARE_API_TOKEN\"
# cloudflare_model = \"typesafe/jev\"

# [providers.openai-chat]
# runner = \"chat\"
# base_url = \"https://api.openai.com/v1\"
# model = \"gpt-5-mini\"
# api_key_env = \"OPENAI_API_KEY\"
# price_usd_per_million_input = 0.25
# price_usd_per_million_output = 2.00

[intake]
# How many questions the `interview` directive may ask a person in a rolling
# 24 hours. At the cap, the worker leaves its intake tasks queued rather than
# start a turn that would ask another; see docs/INTAKE.md.
max_questions_per_day = 8

# Every task carries the trust of its source: operator (forge add and the
# CLI), contact (a known contact through the Signal plugin, the portal, or a
# message trigger), or public (the github-issues plugin, a webhook whose
# caller is not a contact, anything a stranger can send). Each level's own
# table below is the policy it is judged against: per_task_usd (the cost cap
# a task filed at this level gets; --budget may say less, never more without
# --allow-over-trust-cap; unset means [budget]'s own per_task_usd),
# per_initiative_usd (what an initiative's tasks may cost together, unset means
# no cap), workflows (allowed workflow names, unset means every workflow),
# allow_protected, egress (\"model\": only the configured providers' model
# endpoints, or \"declared\": also the hosts forge.toml's own [sandbox] egress
# names), per_day (how many tasks may start at this level per day, unset means
# no cap), and auto_land (may a verified task at this level land itself). The
# caps, workflows, allow_protected and per_day are enforced at enqueue; egress
# and auto_land by a later task. A level that is not operator, or whose egress
# is \"model\", needs a bwrap backend (refused at enqueue, blocked at claim)
# unless allow_unsandboxed = true (forge doctor flags it). See docs/GTM.md.
[trust.operator]
allow_protected = true
egress = \"declared\"
auto_land = true

[trust.contact]
# Reviewed or stricter for a contact's own requested work, plus concierge
# and intake, the front door itself (forge ask's own decision, and the
# interview a need files) — neither ever writes code.
workflows = [\"reviewed\", \"tdd-reviewed\", \"concierge\", \"intake\"]
per_task_usd = 10.0
per_initiative_usd = 50.0
allow_protected = false
egress = \"declared\"
auto_land = true

[trust.public]
# Tighter on every field: a stranger's task costs less, runs under the one
# workflow this operator trusts unattended, cannot land itself, and is
# capped at five a day.
per_task_usd = 5.0
per_initiative_usd = 25.0
workflows = [\"reviewed\"]
allow_protected = false
egress = \"model\"
per_day = 5
auto_land = false

# Which provider each role runs under by default: the four contracts
# (code, tests, review, plan) and the supervisor. \"anthropic\" where a
# role names none. A project can override a role with `forge project set
# --role <role>=<provider>`; a task's own `--provider` wins over both and
# sets every role for that task.
#
# [roles]
# review = \"devhome\"

# What the kernel grants a repository automatically when an attempt fails on
# a missing tool, instead of asking a person: `hosts` the egress proxy may be
# opened to for that worktree (a proxy 403 naming one of them), `cache_paths`
# on the host mounted read-only (a missing toolchain or browser cache under
# one of them). A need outside these is left as a failure or a question.
# Every grant is a decision row by forge, listed by `forge doctor` for 7 days.
# See docs/OPS.md, \"Environment needs\".
#
# [environment]
# hosts = [\"registry.npmjs.org\", \"nodejs.org\"]
# cache_paths = [\"~/.cache/node-gyp\", \"~/.cache/ms-playwright\"]

# A project's secrets, injected as environment for that project's jobs
# (`forge job start`) and never into a prompt (see docs/JOBS.md).
#
# [projects.equitizr.secrets]
# SIGNAL_TOKEN = \"...\"

# A named secret a run workflow's operation step may declare, resolved to an
# environment variable the worker process already has (never a value here).
# Granted only to operator-trust jobs, and only to the step that names it.
#
# [secrets]
# cloudflare_token = { env = \"CLOUDFLARE_API_TOKEN\" }
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
    let explore = build_explore(raw.measure.explore, &providers)?;
    Ok(HomeConfig {
        budget,
        sandbox: SandboxPaths {
            ro: ro.iter().map(|p| expand(p)).collect(),
            rw: rw.iter().map(|p| expand(p)).collect(),
            dependency_cache: raw.sandbox.dependency_cache.as_deref().map(expand),
        },
        supervisor: Supervisor {
            // FORGE_SUPERVISOR=0 turns it off for one process: the e2e
            // suite's default, and an operator's quick switch.
            enabled: raw.supervisor.enabled.unwrap_or(true)
                && super::env("SUPERVISOR").map(|v| v != "0").unwrap_or(true),
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
            operator_usd_per_hour: raw.measure.operator_usd_per_hour,
            attention_minutes_per_question: raw
                .measure
                .attention_minutes_per_question
                .unwrap_or(5.0),
            explore,
        },
        intake: Intake {
            max_questions_per_day: raw.intake.max_questions_per_day.unwrap_or(8),
        },
        trust: build_trust(raw.trust)?,
        providers,
        roles,
        project_secrets: raw
            .projects
            .into_iter()
            .map(|(name, p)| (name, p.secrets))
            .collect(),
        secrets: crate::secrets::build(raw.secrets)?,
        environment: crate::environment::Policy::build(
            raw.environment.hosts,
            raw.environment
                .cache_paths
                .map(|v| v.iter().map(|p| expand(p)).collect()),
        )?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "plugin_dirs = [\"~/.config/forge/plugins\", \"relative/plugins\", \"/opt/forge/plugins\"]\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        assert_eq!(
            c.plugin_dirs,
            vec![
                PathBuf::from(std::env::var("HOME").unwrap()).join(".config/forge/plugins"),
                dir.path().join("relative/plugins"),
                PathBuf::from("/opt/forge/plugins"),
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
        assert_eq!(from_template.trust, from_defaults.trust);
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
}
