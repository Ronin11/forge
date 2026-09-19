//! Workflows and actions are data: one TOML file each under
//! `<FORGE2_HOME>/workflows/` (workflows) and `.../workflows/actions/`
//! (actions). An action is a directive (an LLM step) or an operation (a
//! deterministic step). A workflow is an ordered list of references to
//! actions or to other workflows, spliced inline.
//!
//! Identity is the git blob hash of the file. Latest by default: a task
//! resolves everything once at creation, records every hash and every
//! file's text, and runs from that record, so an edit landing mid-run
//! cannot change a running task, and reverting is git. `check` is the one
//! definition of validity; the engine refuses what it rejects.
//! See docs/ACTIONS.md and docs/WORKFLOWS.md.

use anyhow::{Context, Result, bail};
use croner::Cron;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Names the engine inserts itself; a user operation may not shadow them.
pub const KERNEL_OPS: &[&str] = &["verify", "push", "integrate", "land", "clone"];

/// Contracts the kernel enforces for directives. A directive file names
/// one (default: its own name); any other value is rejected. Many
/// directives over few contracts (docs/ACTIONS.md). Serialized by its
/// lowercase name, which is what the files and the stored JSON carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Contract {
    Code,
    Tests,
    Review,
    Plan,
}

impl Contract {
    pub const ALL: [Contract; 4] = [
        Contract::Code,
        Contract::Tests,
        Contract::Review,
        Contract::Plan,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Contract::Code => "code",
            Contract::Tests => "tests",
            Contract::Review => "review",
            Contract::Plan => "plan",
        }
    }

    pub fn parse(s: &str) -> Option<Contract> {
        Contract::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Whether the directive is expected to change files; a read-only
    /// contract is never faulted for not editing.
    pub fn writes(self) -> bool {
        matches!(self, Contract::Code | Contract::Tests)
    }

    /// Whether the contract's verdict runs checks of its own (L1/L2 for
    /// code, red-on-base for tests). A contract that runs none is judged
    /// by its L0 rows alone: nothing to object to is a pass, not
    /// "unverified".
    pub fn verifies_work(self) -> bool {
        matches!(self, Contract::Code | Contract::Tests)
    }
}

impl std::fmt::Display for Contract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What steps produce and consume: the data-flow vocabulary. `branch` is
/// the clone and every step that changes it; `verdict` is the kernel's
/// verify after a directive; the rest are one step's output shown to a
/// later one. Serialized by its file name.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Product {
    Branch,
    VerifyRef,
    Interface,
    Verdict,
    Review,
    Plan,
    Context,
}

impl Product {
    pub const ALL: [Product; 7] = [
        Product::Branch,
        Product::VerifyRef,
        Product::Interface,
        Product::Verdict,
        Product::Review,
        Product::Plan,
        Product::Context,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Product::Branch => "branch",
            Product::VerifyRef => "verify_ref",
            Product::Interface => "interface",
            Product::Verdict => "verdict",
            Product::Review => "review",
            Product::Plan => "plan",
            Product::Context => "context",
        }
    }

    pub fn parse(s: &str) -> Option<Product> {
        Product::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

impl std::fmt::Display for Product {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an operation may produce. `branch`: it changes the tree, the
/// kernel commits the result and verifies it. `interface`: its stdout is
/// the interface the next code directive is shown. Everything else is a
/// directive's or the kernel's to produce.
pub const OPERATION_PRODUCES: &[Product] = &[Product::Branch, Product::Interface, Product::Context];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Directive,
    Operation,
}

/// A workflow's `kind`: `build` (the default) runs a task to a landing;
/// `run` runs a job to a verified effect instead. See docs/JOBS.md.
/// Serialized by its lowercase name.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowKind {
    #[default]
    Build,
    Run,
}

impl WorkflowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowKind::Build => "build",
            WorkflowKind::Run => "run",
        }
    }
}

impl std::fmt::Display for WorkflowKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What starts a job (docs/JOBS.md, "Trigger"). Serialized by its
/// lowercase name; an unknown value is a TOML deserialize error, refused
/// with the file and line.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerOn {
    Manual,
    Schedule,
    Message,
    Webhook,
    Event,
}

impl TriggerOn {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerOn::Manual => "manual",
            TriggerOn::Schedule => "schedule",
            TriggerOn::Message => "message",
            TriggerOn::Webhook => "webhook",
            TriggerOn::Event => "event",
        }
    }

    /// The field `on` requires alongside it: `schedule` a cron
    /// expression, `message` a contact group, `webhook` a name, `event`
    /// a Forge event type; `manual` needs none.
    fn field(self) -> Option<&'static str> {
        match self {
            TriggerOn::Manual => None,
            TriggerOn::Schedule => Some("cron"),
            TriggerOn::Message => Some("contact"),
            TriggerOn::Webhook => Some("name"),
            TriggerOn::Event => Some("type"),
        }
    }
}

impl std::fmt::Display for TriggerOn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What starts a job, and its one field (docs/JOBS.md, "Trigger").
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trigger {
    pub on: TriggerOn,
    pub cron: Option<String>,
    pub contact: Option<String>,
    pub name: Option<String>,
    pub r#type: Option<String>,
    /// Seconds to wait after the firing event before the job is due
    /// (docs/JOBS.md, "Delayed jobs"): parsed from a duration string
    /// (`s`, `m`, `h`, `d`) at workflow load time. `None` fires the job
    /// immediately, as before this field existed.
    pub delay: Option<i64>,
}

impl Trigger {
    /// Whether this trigger fires for an inbound message from `contact`
    /// (docs/JOBS.md, "Triggers"): `on = "message"` and a `contact` that
    /// is `"*"` or equals it.
    pub fn matches_message(&self, contact: &str) -> bool {
        self.on == TriggerOn::Message
            && self
                .contact
                .as_deref()
                .is_some_and(|c| c == "*" || c == contact)
    }

    /// The value of the one field `on` names, for display.
    pub fn value(&self) -> Option<&str> {
        match self.on {
            TriggerOn::Manual => None,
            TriggerOn::Schedule => self.cron.as_deref(),
            TriggerOn::Message => self.contact.as_deref(),
            TriggerOn::Webhook => self.name.as_deref(),
            TriggerOn::Event => self.r#type.as_deref(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TriggerRaw {
    on: TriggerOn,
    #[serde(default, deserialize_with = "deserialize_cron")]
    cron: Option<String>,
    #[serde(default)]
    contact: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_delay")]
    delay: Option<i64>,
}

/// Parses `cron` with `croner` at load time, so a schedule trigger that can
/// never fire is refused the same way an unknown `on` value is: as a TOML
/// deserialize error, with the file and the line (docs/JOBS.md, "Triggers").
/// The worker's schedule tick (`src/worker.rs`) can then assume every
/// `Trigger::cron` it sees already parses.
fn deserialize_cron<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let cron: Option<String> = Option::deserialize(deserializer)?;
    if let Some(expr) = &cron {
        Cron::from_str(expr)
            .map_err(|e| serde::de::Error::custom(format!("invalid cron {expr:?}: {e}")))?;
    }
    Ok(cron)
}

/// A duration string (a number followed by `s`, `m`, `h`, or `d`) in
/// seconds: `"5m"` is 300, `"1h"` is 3600, `"0s"` is 0. Shared by
/// `[trigger] delay` (validated at workflow load time, below) and `forge
/// job start --delay` (docs/JOBS.md, "Delayed jobs").
pub fn parse_duration(s: &str) -> std::result::Result<i64, String> {
    let bad = || {
        format!("invalid duration {s:?}: expected a number followed by s, m, h, or d, e.g. \"5m\"")
    };
    if s.is_empty() {
        return Err(bad());
    }
    let (digits, unit) = s.split_at(s.len() - 1);
    let secs_per_unit = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err(bad()),
    };
    let n: i64 = digits.parse().map_err(|_| bad())?;
    if n < 0 {
        return Err(bad());
    }
    Ok(n * secs_per_unit)
}

/// Parses `delay` with `parse_duration` at load time, so a delay that
/// cannot be parsed is refused the same way an invalid cron is: as a TOML
/// deserialize error, with the file and the line (docs/JOBS.md, "Delayed
/// jobs").
fn deserialize_delay<'de, D>(deserializer: D) -> std::result::Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let delay: Option<String> = Option::deserialize(deserializer)?;
    delay
        .map(|s| parse_duration(&s).map_err(serde::de::Error::custom))
        .transpose()
}

/// A side effect on the world an operation performs (docs/JOBS.md,
/// "Effect"). Serialized by its lowercase name; an unknown value is a
/// TOML deserialize error, refused with the file and line.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectKind {
    Message,
    Row,
    File,
    Http,
}

impl EffectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EffectKind::Message => "message",
            EffectKind::Row => "row",
            EffectKind::File => "file",
            EffectKind::Http => "http",
        }
    }
}

impl std::fmt::Display for EffectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What to do when a job's assertions fail (docs/JOBS.md, "Limits").
/// `retry:N` carries its count; the rest are unit values. Serialized as
/// the string form (`ask:contact`, `retry:2`, ...); an unknown value is
/// refused with the file and line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OnFailure {
    AskContact,
    AskOperator,
    Retry(u32),
    Drop,
}

impl OnFailure {
    pub fn as_str(&self) -> String {
        match self {
            OnFailure::AskContact => "ask:contact".to_string(),
            OnFailure::AskOperator => "ask:operator".to_string(),
            OnFailure::Retry(n) => format!("retry:{n}"),
            OnFailure::Drop => "drop".to_string(),
        }
    }

    pub fn parse(s: &str) -> Option<OnFailure> {
        match s {
            "ask:contact" => Some(OnFailure::AskContact),
            "ask:operator" => Some(OnFailure::AskOperator),
            "drop" => Some(OnFailure::Drop),
            _ => s
                .strip_prefix("retry:")
                .and_then(|n| n.parse::<u32>().ok())
                .map(OnFailure::Retry),
        }
    }
}

impl std::fmt::Display for OnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl Serialize for OnFailure {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        s.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for OnFailure {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        OnFailure::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "on_failure {s:?} is not ask:contact, ask:operator, retry:N, or drop"
            ))
        })
    }
}

/// A run workflow's budget and failure policy (docs/JOBS.md, "Limits").
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub budget_usd: f64,
    pub per_day: u32,
    pub on_failure: OnFailure,
    /// The bound on a directive step's inputs (the input document plus
    /// every earlier step's output, as text) in bytes; default 32 kB
    /// (docs/JOBS.md, "Steps").
    #[serde(default = "default_input_bytes")]
    pub input_bytes: usize,
}

pub fn default_input_bytes() -> usize {
    32 * 1024
}

/// How much of an operation's stdout and stderr the kernel keeps on its
/// `ops` row: `tail`, the last 40 lines, or `full`, the whole thing capped
/// at 1 MB.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    #[default]
    Tail,
    Full,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionRaw {
    name: String,
    kind: Kind,
    #[serde(default)]
    description: String,
    #[serde(default)]
    consumes: Vec<String>,
    #[serde(default)]
    produces: Vec<String>,
    model: Option<String>,
    max_turns: Option<u32>,
    timeout_secs: Option<u32>,
    /// Operation: a command to run in the sandbox against the tree.
    run: Option<Vec<String>>,
    /// Operation: run the repository's declared check of this name instead.
    check: Option<String>,
    /// Directive: which kernel contract runs it (default: the name).
    contract: Option<String>,
    /// Directive (code contract): paths the agent may change; a file, or a
    /// directory with a trailing slash, or a suffix like `*.md`.
    #[serde(default)]
    paths: Vec<String>,
    /// Directive: a short instruction appended to the task text.
    #[serde(default)]
    brief: String,
    /// Directive: text appended verbatim as a final "This step:" section
    /// of the role prompt the agent receives.
    prompt: Option<String>,
    /// Directive: the JSON Schema a job step's structured output is
    /// validated against before the next step sees it (docs/JOBS.md,
    /// "Steps"). Unused by a build workflow, which holds every directive to
    /// the envelope schema instead.
    schema: Option<String>,
    /// Directive (plan contract): when true and the task has an
    /// initiative id, file the plan's items as sibling tasks in that
    /// initiative after this step, instead of running the code step in
    /// the same task.
    #[serde(default)]
    file_into_initiative: bool,
    /// Operation: run with the verification namespace overlaid from the
    /// trusted refs (a hidden suite the coder never sees).
    #[serde(default)]
    overlay: bool,
    /// Operation: its failure is the preceding directive's failure, fed
    /// back as a retry, rather than a one-shot task failure.
    #[serde(default)]
    verifies: bool,
    /// Operation: how much of its stdout and stderr the kernel keeps.
    #[serde(default)]
    output: Output,
}

/// One action file, one version.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionDef {
    pub name: String,
    pub kind: Kind,
    pub description: String,
    pub consumes: Vec<Product>,
    pub produces: Vec<Product>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
    pub run: Option<Vec<String>>,
    pub check: Option<String>,
    pub contract: Contract,
    pub paths: Vec<String>,
    pub brief: String,
    pub prompt: Option<String>,
    pub schema: Option<String>,
    pub file_into_initiative: bool,
    pub overlay: bool,
    pub verifies: bool,
    pub output: Output,
    pub hash: String,
    pub text: String,
}

impl ActionDef {
    /// An operation that changes the tree: the kernel commits what it
    /// changed and verifies the result, as it does after a directive.
    pub fn mutates(&self) -> bool {
        self.kind == Kind::Operation && self.produces.contains(&Product::Branch)
    }
    /// An operation whose stdout becomes the interface the coder is shown.
    /// `context`: its stdout is shown to the next directive as a map of
    /// where things are, cut to a budget and recorded in the attempt.
    pub fn yields_context(&self) -> bool {
        self.kind == Kind::Operation && self.produces.contains(&Product::Context)
    }

    pub fn yields_interface(&self) -> bool {
        self.kind == Kind::Operation && self.produces.contains(&Product::Interface)
    }
    /// An operation that reads the task's hidden tests: it runs in a
    /// scratch copy of base with the verify ref overlaid, never in the
    /// coder's clone.
    pub fn reads_verify_ref(&self) -> bool {
        self.kind == Kind::Operation && self.consumes.contains(&Product::VerifyRef)
    }
    /// The operation stores its whole stdout and stderr, capped at 1 MB,
    /// rather than the 40-line tail.
    pub fn output_full(&self) -> bool {
        self.output == Output::Full
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct StepRaw {
    action: Option<String>,
    workflow: Option<String>,
    /// Old spelling of `action`, accepted for one release.
    kind: Option<String>,
    model: Option<String>,
    max_turns: Option<u32>,
    timeout_secs: Option<u32>,
    /// A job step's directive: the role it is routed under (docs/JOBS.md,
    /// "Steps").
    role: Option<String>,
    /// A job step's operation: the effect it performs (docs/JOBS.md,
    /// "Effects").
    effect: Option<EffectKind>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowRaw {
    name: String,
    /// `build` (the default) or `run` (docs/JOBS.md).
    #[serde(default)]
    kind: WorkflowKind,
    #[serde(default)]
    description: String,
    steps: Vec<StepRaw>,
    /// Run the `assess` directive after this workflow lands, storing its
    /// score and findings on the `assessments` table; never on a step
    /// list, never blocking, never changing task state (see
    /// docs/ACTIONS.md, "Assessment"). Default false.
    #[serde(default)]
    assess: bool,
    #[serde(default)]
    meta: Meta,
    /// `kind = "run"` only: what starts a job.
    trigger: Option<TriggerRaw>,
    /// `kind = "run"` only: named commands run after the steps, with the
    /// effect log and every step's output on disk.
    #[serde(default)]
    assert: BTreeMap<String, Vec<String>>,
    /// `kind = "run"` only: named commands run in the scratch tree with the
    /// job's environment before any step; the first to exit 0 ends the job
    /// `Skipped` with the first line of its stdout as the reason (docs/JOBS.md,
    /// "Skipping a run").
    #[serde(default)]
    skip_if: BTreeMap<String, Vec<String>>,
    /// `kind = "run"` only: budget and failure policy.
    limits: Option<Limits>,
    /// `kind = "run"` only: extra environment for every operation step (and
    /// `[skip_if]` command) of this workflow's jobs — a threshold, a table
    /// name, a URL — declared here instead of hard-coded in the action's
    /// script, so changing a number is a workflow-file edit, not a script
    /// edit.
    #[serde(default)]
    env: BTreeMap<String, String>,
}

/// What the author declares about a workflow, for a human or an agent
/// choosing one. Declared, never measured: measured numbers live in the
/// stats table and are merged in at read time.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default)]
    pub use_when: String,
    #[serde(default)]
    pub avoid_when: String,
    #[serde(default)]
    pub requires: Vec<String>,
    /// Ignored. Costs are measured from runs, never declared; the field is
    /// accepted so older files still load, and `check` warns about it.
    #[serde(default, skip_serializing)]
    pub cost_factor: Option<f64>,
}

/// A step as written: a reference plus per-step overrides.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepRef {
    pub action: Option<String>,
    pub workflow: Option<String>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
    pub role: Option<String>,
    pub effect: Option<EffectKind>,
}

#[derive(Clone, Debug)]
pub struct Workflow {
    pub name: String,
    pub kind: WorkflowKind,
    pub description: String,
    pub steps: Vec<StepRef>,
    /// Run the `assess` directive after a landing on this workflow (see
    /// `WorkflowRaw::assess`).
    pub assess: bool,
    pub meta: Meta,
    /// `kind = "run"` only.
    pub trigger: Option<Trigger>,
    /// `kind = "run"` only; empty when unset.
    pub assert: BTreeMap<String, Vec<String>>,
    /// `kind = "run"` only; empty when unset (docs/JOBS.md, "Skipping a
    /// run").
    pub skip_if: BTreeMap<String, Vec<String>>,
    /// `kind = "run"` only.
    pub limits: Option<Limits>,
    /// `kind = "run"` only; empty when unset.
    pub env: BTreeMap<String, String>,
    pub hash: String,
    pub path: PathBuf,
    pub text: String,
}

/// A step after resolution: the exact action version it will run, with
/// the overrides that apply, and the workflow path it came through.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedStep {
    pub action: ActionDef,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
    pub via: Vec<String>,
}

/// One version that a task ran under.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pin {
    pub kind: String,
    pub name: String,
    pub hash: String,
}

/// Everything a task needs to run, recorded at creation.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Resolved {
    pub steps: Vec<ResolvedStep>,
    pub pins: Vec<Pin>,
}

impl Workflow {
    pub fn steps_text(&self) -> String {
        self.steps
            .iter()
            .map(|s| {
                s.action
                    .clone()
                    .or_else(|| s.workflow.as_ref().map(|w| format!("({w})")))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" → ")
    }
}

const BUILTIN_ACTIONS: &[(&str, &str)] = &[
    ("code.toml", include_str!("builtins/actions/code.toml")),
    ("tests.toml", include_str!("builtins/actions/tests.toml")),
    (
        "investigate.toml",
        include_str!("builtins/actions/investigate.toml"),
    ),
    ("review.toml", include_str!("builtins/actions/review.toml")),
    ("docs.toml", include_str!("builtins/actions/docs.toml")),
    ("fix.toml", include_str!("builtins/actions/fix.toml")),
    ("polish.toml", include_str!("builtins/actions/polish.toml")),
    (
        "document.toml",
        include_str!("builtins/actions/document.toml"),
    ),
    ("graph.toml", include_str!("builtins/actions/graph.toml")),
    (
        "playwright.toml",
        include_str!("builtins/actions/playwright.toml"),
    ),
    ("setup.toml", include_str!("builtins/actions/setup.toml")),
    (
        "interview.toml",
        include_str!("builtins/actions/interview.toml"),
    ),
    ("assess.toml", include_str!("builtins/actions/assess.toml")),
    (
        "deploy-look.toml",
        include_str!("builtins/actions/deploy-look.toml"),
    ),
    (
        "concierge.toml",
        include_str!("builtins/actions/concierge.toml"),
    ),
];

const BUILTIN_OPERATIONS: &[(&str, &str)] = &[
    (
        "comments-only.toml",
        include_str!("builtins/operations/comments-only.toml"),
    ),
    (
        "graph-check.toml",
        include_str!("builtins/operations/graph-check.toml"),
    ),
    (
        "repo-map.toml",
        include_str!("builtins/operations/repo-map.toml"),
    ),
    (
        "diff-size.toml",
        include_str!("builtins/operations/diff-size.toml"),
    ),
    ("fmt.toml", include_str!("builtins/operations/fmt.toml")),
    (
        "interface.toml",
        include_str!("builtins/operations/interface.toml"),
    ),
    (
        "deploy-command.toml",
        include_str!("builtins/operations/deploy-command.toml"),
    ),
    (
        "deploy-user-service.toml",
        include_str!("builtins/operations/deploy-user-service.toml"),
    ),
    (
        "deploy-static.toml",
        include_str!("builtins/operations/deploy-static.toml"),
    ),
    (
        "deploy-smoke.toml",
        include_str!("builtins/operations/deploy-smoke.toml"),
    ),
    (
        "provision-hetzner.toml",
        include_str!("builtins/operations/provision-hetzner.toml"),
    ),
    (
        "write-file.toml",
        include_str!("builtins/operations/write-file.toml"),
    ),
    (
        "append-row.toml",
        include_str!("builtins/operations/append-row.toml"),
    ),
    (
        "http-post.toml",
        include_str!("builtins/operations/http-post.toml"),
    ),
    (
        "send-signal.toml",
        include_str!("builtins/operations/send-signal.toml"),
    ),
];

const BUILTIN_WORKFLOWS: &[(&str, &str)] = &[
    (
        "planned.toml",
        include_str!("builtins/workflows/planned.toml"),
    ),
    (
        "direct.toml",
        include_str!("builtins/workflows/direct.toml"),
    ),
    ("tdd.toml", include_str!("builtins/workflows/tdd.toml")),
    ("docs.toml", include_str!("builtins/workflows/docs.toml")),
    ("cheap.toml", include_str!("builtins/workflows/cheap.toml")),
    (
        "polish.toml",
        include_str!("builtins/workflows/polish.toml"),
    ),
    (
        "reviewed.toml",
        include_str!("builtins/workflows/reviewed.toml"),
    ),
    (
        "playable.toml",
        include_str!("builtins/workflows/playable.toml"),
    ),
    (
        "documented.toml",
        include_str!("builtins/workflows/documented.toml"),
    ),
    (
        "mapped.toml",
        include_str!("builtins/workflows/mapped.toml"),
    ),
    (
        "tdd-reviewed.toml",
        include_str!("builtins/workflows/tdd-reviewed.toml"),
    ),
    (
        "intake.toml",
        include_str!("builtins/workflows/intake.toml"),
    ),
    (
        "concierge.toml",
        include_str!("builtins/workflows/concierge.toml"),
    ),
];

fn dir_of(home: &Path) -> PathBuf {
    home.join("workflows")
}

/// The directory exists, is a git repository, and holds the built-ins if
/// it holds nothing at all.
fn ensure(home: &Path) -> Result<PathBuf> {
    let dir = dir_of(home);
    let actions = dir.join("actions");
    std::fs::create_dir_all(&actions)?;
    if !dir.join(".git").exists() {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["init", "-q"])
            .output()?;
        if !o.status.success() {
            bail!(
                "git init in {} failed: {}",
                dir.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
    }
    // Built-ins are written when missing and never overwritten: an existing
    // install gains new built-ins on upgrade and keeps its own edits.
    for (file, text) in BUILTIN_WORKFLOWS {
        let p = dir.join(file);
        if !p.exists() {
            std::fs::write(&p, text)?;
        }
    }
    for (file, text) in BUILTIN_ACTIONS.iter().chain(BUILTIN_OPERATIONS) {
        let p = actions.join(file);
        if !p.exists() {
            std::fs::write(&p, text)?;
        }
    }
    Ok(dir)
}

/// The git blob hash of a file: the identity git gives this version.
fn blob_hash(dir: &Path, path: &Path) -> Result<String> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("hash-object")
        .arg(path)
        .output()?;
    if !o.status.success() {
        bail!(
            "git hash-object {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn parse_action(path: &Path, text: &str, hash: String) -> Result<ActionDef> {
    let raw: ActionRaw =
        toml::from_str(text).with_context(|| format!("parsing {}", path.display()))?;
    let stem = path.file_stem().unwrap().to_string_lossy();
    if raw.name != stem {
        bail!(
            "{}: name {:?} does not match the file name {:?}",
            path.display(),
            raw.name,
            stem
        );
    }
    if raw.max_turns == Some(0) || raw.timeout_secs == Some(0) {
        bail!(
            "{}: max_turns and timeout_secs must be positive",
            path.display()
        );
    }
    match raw.kind {
        Kind::Operation => {
            if raw.run.is_none() == raw.check.is_none() {
                bail!(
                    "{}: an operation needs exactly one of `run` or `check`",
                    path.display()
                );
            }
            if raw.run.as_ref().is_some_and(|r| r.is_empty()) {
                bail!("{}: `run` is empty", path.display());
            }
        }
        Kind::Directive => {
            if raw.run.is_some() || raw.check.is_some() {
                bail!(
                    "{}: a directive does not have `run` or `check`",
                    path.display()
                );
            }
            let name = raw.contract.clone().unwrap_or_else(|| raw.name.clone());
            let Some(contract) = Contract::parse(&name) else {
                bail!(
                    "{}: directive contract {:?} is not one the kernel enforces (known: {})",
                    path.display(),
                    name,
                    Contract::ALL.map(Contract::as_str).join(", ")
                );
            };
            if !raw.paths.is_empty() && contract != Contract::Code {
                bail!(
                    "{}: `paths` applies to the code contract only",
                    path.display()
                );
            }
            if raw.file_into_initiative && contract != Contract::Plan {
                bail!(
                    "{}: `file_into_initiative` applies to the plan contract only",
                    path.display()
                );
            }
        }
    }
    if raw.kind == Kind::Operation
        && (raw.contract.is_some()
            || !raw.paths.is_empty()
            || !raw.brief.is_empty()
            || raw.prompt.is_some()
            || raw.schema.is_some()
            || raw.file_into_initiative)
    {
        bail!(
            "{}: contract, paths, brief, prompt, schema, and file_into_initiative apply to directives only",
            path.display()
        );
    }
    if let Some(schema) = &raw.schema {
        let v: serde_json::Value = serde_json::from_str(schema)
            .with_context(|| format!("{}: `schema` is not valid JSON", path.display()))?;
        jsonschema::validator_for(&v)
            .with_context(|| format!("{}: `schema` is not a valid JSON Schema", path.display()))?;
    }
    if raw.kind == Kind::Directive && (raw.overlay || raw.verifies) {
        bail!(
            "{}: overlay and verifies apply to operations only",
            path.display()
        );
    }
    if raw.kind == Kind::Directive && raw.output != Output::Tail {
        bail!("{}: `output` applies to operations only", path.display());
    }
    if raw.kind == Kind::Operation
        && let Some(p) = raw
            .produces
            .iter()
            .find(|p| !Product::parse(p).is_some_and(|p| OPERATION_PRODUCES.contains(&p)))
    {
        bail!(
            "{}: an operation cannot produce {:?}; it may produce {}",
            path.display(),
            p,
            OPERATION_PRODUCES
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if raw.kind == Kind::Operation && raw.model.is_some() {
        bail!("{}: `model` applies to directives only", path.display());
    }
    // Directives were validated above; an operation's contract is its name
    // and never consulted. Products come from a closed vocabulary.
    let contract = raw
        .contract
        .as_deref()
        .and_then(Contract::parse)
        .or_else(|| Contract::parse(&raw.name))
        .unwrap_or(Contract::Code);
    let products = |list: &[String], field: &str| -> Result<Vec<Product>> {
        list.iter()
            .map(|p| {
                Product::parse(p).with_context(|| {
                    format!(
                        "{}: `{field}` names {p:?}, which is not a product; the products are {}",
                        path.display(),
                        Product::ALL.map(Product::as_str).join(", ")
                    )
                })
            })
            .collect()
    };
    let consumes = products(&raw.consumes, "consumes")?;
    let produces = products(&raw.produces, "produces")?;
    Ok(ActionDef {
        name: raw.name,
        kind: raw.kind,
        description: raw.description,
        consumes,
        produces,
        model: raw.model,
        max_turns: raw.max_turns,
        timeout_secs: raw.timeout_secs,
        run: raw.run,
        check: raw.check,
        contract,
        paths: raw.paths,
        brief: raw.brief,
        prompt: raw.prompt,
        schema: raw.schema,
        file_into_initiative: raw.file_into_initiative,
        overlay: raw.overlay,
        verifies: raw.verifies,
        output: raw.output,
        hash,
        text: text.to_string(),
    })
}

fn parse_workflow(path: &Path, text: &str, hash: String) -> Result<Workflow> {
    let raw: WorkflowRaw =
        toml::from_str(text).with_context(|| format!("parsing {}", path.display()))?;
    let stem = path.file_stem().unwrap().to_string_lossy();
    if raw.name != stem {
        bail!(
            "{}: name {:?} does not match the file name {:?}",
            path.display(),
            raw.name,
            stem
        );
    }
    if raw.steps.is_empty() {
        bail!("{}: a workflow needs at least one step", path.display());
    }
    match raw.kind {
        WorkflowKind::Run => {
            if raw.trigger.is_none() {
                bail!(
                    "{}: kind = \"run\" needs a [trigger] (docs/JOBS.md)",
                    path.display()
                );
            }
        }
        WorkflowKind::Build => {
            if raw.trigger.is_some() {
                bail!(
                    "{}: kind = \"build\" (the default) may not have [trigger]; that is a run workflow's section (set kind = \"run\")",
                    path.display()
                );
            }
            if !raw.assert.is_empty() {
                bail!(
                    "{}: kind = \"build\" (the default) may not have [assert]; that is a run workflow's section (set kind = \"run\")",
                    path.display()
                );
            }
            if !raw.skip_if.is_empty() {
                bail!(
                    "{}: kind = \"build\" (the default) may not have [skip_if]; that is a run workflow's section (set kind = \"run\")",
                    path.display()
                );
            }
            if raw.limits.is_some() {
                bail!(
                    "{}: kind = \"build\" (the default) may not have [limits]; that is a run workflow's section (set kind = \"run\")",
                    path.display()
                );
            }
            if !raw.env.is_empty() {
                bail!(
                    "{}: kind = \"build\" (the default) may not have [env]; that is a run workflow's section (set kind = \"run\")",
                    path.display()
                );
            }
        }
    }
    let trigger = raw.trigger.map(|t| build_trigger(path, t)).transpose()?;
    let mut steps = Vec::new();
    for s in raw.steps {
        let action = s.action.or(s.kind);
        if action.is_some() == s.workflow.is_some() {
            bail!(
                "{}: a step names exactly one of `action` or `workflow`",
                path.display()
            );
        }
        if s.max_turns == Some(0) || s.timeout_secs == Some(0) {
            bail!(
                "{}: max_turns and timeout_secs must be positive",
                path.display()
            );
        }
        steps.push(StepRef {
            action,
            workflow: s.workflow,
            model: s.model,
            max_turns: s.max_turns,
            timeout_secs: s.timeout_secs,
            role: s.role,
            effect: s.effect,
        });
    }
    Ok(Workflow {
        name: raw.name,
        kind: raw.kind,
        description: raw.description,
        steps,
        assess: raw.assess,
        meta: raw.meta,
        trigger,
        assert: raw.assert,
        skip_if: raw.skip_if,
        limits: raw.limits,
        env: raw.env,
        hash,
        path: path.to_path_buf(),
        text: text.to_string(),
    })
}

/// Validate a `[trigger]` table: the one field `on` requires is present
/// and non-empty, and no other trigger field is set.
fn build_trigger(path: &Path, raw: TriggerRaw) -> Result<Trigger> {
    let fields: [(&str, &Option<String>); 4] = [
        ("cron", &raw.cron),
        ("contact", &raw.contact),
        ("name", &raw.name),
        ("type", &raw.r#type),
    ];
    let want = raw.on.field();
    for (field, val) in fields {
        let wanted = Some(field) == want;
        if wanted && val.as_deref().is_none_or(str::is_empty) {
            bail!(
                "{}: [trigger] on = \"{}\" needs `{field}`",
                path.display(),
                raw.on.as_str()
            );
        }
        if !wanted && val.is_some() {
            bail!(
                "{}: [trigger] on = \"{}\" does not take `{field}`",
                path.display(),
                raw.on.as_str()
            );
        }
    }
    Ok(Trigger {
        on: raw.on,
        cron: raw.cron,
        contact: raw.contact,
        name: raw.name,
        r#type: raw.r#type,
        delay: raw.delay,
    })
}

fn toml_files(d: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    v.sort();
    Ok(v)
}

/// Every action and every workflow, loaded once: one read of each
/// directory, one git blob hash per file. A file that fails to parse
/// contributes a problem instead of aborting the load, so one bad file
/// does not hide the rest. `check` adds the problems that need every
/// action and workflow already loaded (kernel shadowing, and resolving
/// every workflow) on top of this.
pub struct Catalog {
    pub workflows: BTreeMap<String, Workflow>,
    pub actions: BTreeMap<String, ActionDef>,
    pub problems: Vec<Problem>,
}

fn load_dir<T>(
    files: Vec<PathBuf>,
    file_of: impl Fn(&Path) -> String,
    mut parse: impl FnMut(&Path, &str) -> Result<T>,
) -> Result<(Vec<T>, Vec<Problem>)> {
    let mut items = Vec::new();
    let mut problems = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path)?;
        match parse(&path, &text) {
            Ok(item) => items.push(item),
            Err(e) => problems.push(Problem {
                file: file_of(&path),
                blocking: true,
                what: format!("{e:#}"),
            }),
        }
    }
    Ok((items, problems))
}

pub fn load_catalog(home: &Path) -> Result<Catalog> {
    let dir = ensure(home)?;
    let (raw_actions, mut problems) = load_dir(
        toml_files(&dir.join("actions"))?,
        |p| format!("actions/{}", p.file_name().unwrap().to_string_lossy()),
        |p, t| parse_action(p, t, blob_hash(&dir, p)?),
    )?;
    let mut actions = BTreeMap::new();
    for a in raw_actions {
        if a.description.trim().is_empty() {
            problems.push(Problem {
                file: format!("actions/{}.toml", a.name),
                blocking: false,
                what: "no description".into(),
            });
        }
        actions.insert(a.name.clone(), a);
    }

    let (raw_workflows, wf_problems) = load_dir(
        toml_files(&dir)?,
        |p| p.file_name().unwrap().to_string_lossy().into_owned(),
        |p, t| parse_workflow(p, t, blob_hash(&dir, p)?),
    )?;
    problems.extend(wf_problems);
    let mut workflows = BTreeMap::new();
    for w in raw_workflows {
        let file = format!("{}.toml", w.name);
        if w.meta.cost_factor.is_some() {
            problems.push(Problem {
                file: file.clone(),
                blocking: false,
                what: "[meta] cost_factor is ignored: costs are measured from runs, never declared; remove it".into(),
            });
        }
        if w.description.trim().is_empty() {
            problems.push(Problem {
                file: file.clone(),
                blocking: false,
                what: "no description".into(),
            });
        }
        if w.meta.use_when.trim().is_empty() || w.meta.avoid_when.trim().is_empty() {
            problems.push(Problem {
                file,
                blocking: false,
                what: "[meta] use_when and avoid_when are empty; a chooser has nothing to read"
                    .into(),
            });
        }
        workflows.insert(w.name.clone(), w);
    }

    Ok(Catalog {
        workflows,
        actions,
        problems,
    })
}

/// Every problem in a catalog that means a file failed to load at all
/// (as opposed to a convention `check` alone enforces, like kernel
/// shadowing): the same gate `load_all`, `load_actions`, `resolve`, and
/// `get` used to get for free from `?` on a per-file parse.
fn ensure_sound(cat: &Catalog) -> Result<()> {
    if let Some(p) = cat.problems.iter().find(|p| p.blocking) {
        bail!("{}", p.what);
    }
    Ok(())
}

/// Every action file, by name.
pub fn load_actions(home: &Path) -> Result<BTreeMap<String, ActionDef>> {
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    Ok(cat.actions)
}

/// Every workflow, sorted by name.
pub fn load_all(home: &Path) -> Result<Vec<Workflow>> {
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    Ok(cat.workflows.into_values().collect())
}

pub fn get(home: &Path, name: &str) -> Result<Option<Workflow>> {
    let mut cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    Ok(cat.workflows.remove(name))
}

fn splice(
    wf: &Workflow,
    workflows: &BTreeMap<String, Workflow>,
    actions: &BTreeMap<String, ActionDef>,
    path: &mut Vec<String>,
    out: &mut Resolved,
) -> Result<()> {
    if path.contains(&wf.name) {
        bail!(
            "workflow {:?} references itself through {}",
            wf.name,
            path.join(" → ")
        );
    }
    path.push(wf.name.clone());
    let pin = Pin {
        kind: "workflow".into(),
        name: wf.name.clone(),
        hash: wf.hash.clone(),
    };
    if !out.pins.contains(&pin) {
        out.pins.push(pin);
    }
    for s in &wf.steps {
        if let Some(name) = &s.workflow {
            let child = workflows.get(name).with_context(|| {
                format!(
                    "workflow {:?} references unknown workflow {:?}",
                    wf.name, name
                )
            })?;
            splice(child, workflows, actions, path, out)?;
        } else if let Some(name) = s.action.as_deref() {
            let a = actions.get(name).with_context(|| {
                format!(
                    "workflow {:?} references unknown action {:?}",
                    wf.name, name
                )
            })?;

            let pin = Pin {
                kind: "action".into(),
                name: a.name.clone(),
                hash: a.hash.clone(),
            };
            if !out.pins.contains(&pin) {
                out.pins.push(pin);
            }
            out.steps.push(ResolvedStep {
                action: a.clone(),
                model: s.model.clone().or_else(|| a.model.clone()),
                max_turns: s.max_turns.or(a.max_turns),
                timeout_secs: s.timeout_secs.or(a.timeout_secs),
                via: path.clone(),
            });
        }
    }
    path.pop();
    Ok(())
}

/// The data-flow rule: every step's `consumes` must already be produced.
/// Kernel verify after each directive produces `verdict`; the clone
/// produces `branch`.
fn check_flow(steps: &[ResolvedStep]) -> Result<()> {
    let mut have: BTreeSet<Product> = [Product::Branch].into_iter().collect();
    for (i, s) in steps.iter().enumerate() {
        for c in &s.action.consumes {
            if !have.contains(c) {
                bail!(
                    "step {} ({}) consumes {:?}, which nothing before it produces (have: {})",
                    i + 1,
                    s.action.name,
                    c.as_str(),
                    have.iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        for p in &s.action.produces {
            have.insert(*p);
        }
        if s.action.kind == Kind::Directive || s.action.mutates() {
            have.insert(Product::Verdict);
            if s.action.contract == Contract::Review {
                have.insert(Product::Review);
            }
        }
    }
    if !steps.iter().any(|s| s.action.kind == Kind::Directive) {
        bail!("a workflow needs at least one directive; operations alone produce nothing to push");
    }
    for (i, s) in steps.iter().enumerate() {
        if s.action.kind == Kind::Operation
            && s.action.verifies
            && !steps[..i].iter().any(|p| p.action.kind == Kind::Directive)
        {
            bail!(
                "step {} ({}) verifies the preceding directive, but no directive precedes it",
                i + 1,
                s.action.name
            );
        }
    }
    Ok(())
}

/// Resolve a workflow by name into the exact versions a task will run,
/// splicing referenced workflows inline. Fails on unknown references,
/// cycles, and data-flow violations.
pub fn resolve(home: &Path, name: &str) -> Result<Resolved> {
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    let wf = cat
        .workflows
        .get(name)
        .with_context(|| format!("unknown workflow {name:?}; see `forge workflows`"))?;
    let mut out = Resolved::default();
    splice(wf, &cat.workflows, &cat.actions, &mut Vec::new(), &mut out)?;
    check_flow(&out.steps)?;
    Ok(out)
}

/// A job step after resolution: the action it names, plus its own `role`
/// (a directive, routed to a provider) and any per-step overrides
/// (docs/JOBS.md, "Steps"). An operation step's `effect` is validated here
/// (see `job_steps`) but carries no further meaning to the executor: what
/// an operation actually did is read back from the effect log it writes,
/// not from what it declared.
#[derive(Clone, Debug)]
pub struct RunStep {
    pub action: ActionDef,
    pub role: Option<String>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
}

/// A job step, resolved to the action it names (docs/JOBS.md, "Steps"). A
/// step may instead name a sibling `kind = "run"` workflow (`workflow =
/// "…"`, the same field a build workflow splices in); its steps are
/// inlined here, recursively, the way a build workflow's own splice works,
/// so a daily automation can be assembled from smaller run workflows
/// (docs/JOBS.md, "Steps") — `doctor-daily` splicing in `disk-and-logs` is
/// the motivating case. A cycle, or a reference to a workflow that is not
/// itself `kind = "run"`, is refused. A directive step must name a `role`
/// (routed to a provider like every role) and its action must declare a
/// `schema`; an operation step must not name a `role`, and a directive
/// step must not name an `effect`.
fn job_steps(
    wf: &Workflow,
    workflows: &BTreeMap<String, Workflow>,
    actions: &BTreeMap<String, ActionDef>,
) -> Result<Vec<RunStep>> {
    let mut out = Vec::new();
    job_steps_into(wf, workflows, actions, &mut Vec::new(), &mut out)?;
    Ok(out)
}

fn job_steps_into(
    wf: &Workflow,
    workflows: &BTreeMap<String, Workflow>,
    actions: &BTreeMap<String, ActionDef>,
    path: &mut Vec<String>,
    out: &mut Vec<RunStep>,
) -> Result<()> {
    if path.contains(&wf.name) {
        bail!(
            "run workflow {:?} references itself through {}",
            wf.name,
            path.join(" → ")
        );
    }
    path.push(wf.name.clone());
    for s in &wf.steps {
        if let Some(name) = &s.workflow {
            let child = workflows.get(name).with_context(|| {
                format!("{:?}: job step names unknown workflow {name:?}", wf.name)
            })?;
            if child.kind != WorkflowKind::Run {
                bail!(
                    "{:?}: job step names workflow {name:?}, which is kind = \"build\"; a run workflow may only splice in another run workflow",
                    wf.name
                );
            }
            job_steps_into(child, workflows, actions, path, out)?;
            continue;
        }
        let name = s.action.as_deref().with_context(|| {
            format!(
                "{:?}: a job step names exactly one of `action` or `workflow`",
                wf.name
            )
        })?;
        let action = actions
            .get(name)
            .cloned()
            .with_context(|| format!("{:?}: job step names unknown action {name:?}", wf.name))?;
        match action.kind {
            Kind::Directive => {
                if s.role.as_deref().is_none_or(|r| r.trim().is_empty()) {
                    bail!(
                        "{:?}: job step {name:?} is a directive; it needs `role` (docs/JOBS.md, \"Steps\")",
                        wf.name
                    );
                }
                if action.schema.as_deref().is_none_or(|s| s.trim().is_empty()) {
                    bail!(
                        "{:?}: job step {name:?} is a directive; its action {name:?} needs a `schema` (docs/JOBS.md, \"Steps\")",
                        wf.name
                    );
                }
                if s.effect.is_some() {
                    bail!(
                        "{:?}: job step {name:?} is a directive; `effect` applies to operation steps only",
                        wf.name
                    );
                }
            }
            Kind::Operation if s.role.is_some() => {
                bail!(
                    "{:?}: job step {name:?} is an operation; `role` applies to directive steps only",
                    wf.name
                );
            }
            Kind::Operation => {}
        }
        out.push(RunStep {
            model: s.model.clone().or_else(|| action.model.clone()),
            max_turns: s.max_turns.or(action.max_turns),
            timeout_secs: s.timeout_secs.or(action.timeout_secs),
            role: s.role.clone(),
            action,
        });
    }
    path.pop();
    Ok(())
}

/// A run workflow by name, resolved to the exact action each of its steps
/// runs (docs/JOBS.md, "The executor"). Unlike `resolve`, there is none of
/// `check_flow`'s build-only data-flow rules: a job step's action is used
/// as written, though a step may splice in a sibling run workflow (see
/// `job_steps`). Fails on an unknown workflow, a workflow that is not
/// `kind = "run"`, an unknown action, or a step that names a workflow this
/// catalog does not have.
pub fn resolve_job(home: &Path, name: &str) -> Result<(Workflow, Vec<RunStep>)> {
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    let wf = cat
        .workflows
        .get(name)
        .with_context(|| format!("unknown workflow {name:?}; see `forge workflows`"))?
        .clone();
    if wf.kind != WorkflowKind::Run {
        bail!("{name:?} is kind = \"build\"; `forge job start` runs kind = \"run\" workflows only");
    }
    let steps = job_steps(&wf, &cat.workflows, &cat.actions)?;
    Ok((wf, steps))
}

/// A run workflow read straight from a project's own repository, at
/// `.forge/workflows/<name>.toml` with its actions under
/// `.forge/workflows/actions/` (docs/JOBS.md, "Where an automation
/// lives"), rather than the operator's catalog `resolve_job` reads. Every
/// sibling `.forge/workflows/*.toml` is parsed too, so a step that splices
/// in another run workflow (`job_steps`) resolves against the repository's
/// own. `forge job bench` uses this: it measures an automation that is
/// checked into the project it belongs to, not a built-in. No `ensure`:
/// this directory is the project's own and is never git-initialised or
/// seeded with built-ins the way the operator's catalog is.
pub fn resolve_job_in_repo(repo: &Path, name: &str) -> Result<(Workflow, Vec<RunStep>)> {
    let dir = repo.join(".forge").join("workflows");
    let path = dir.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("no {} (see docs/JOBS.md)", path.display()))?;
    let wf = parse_workflow(&path, &text, blob_hash(repo, &path)?)?;
    if wf.kind != WorkflowKind::Run {
        bail!("{name:?} is kind = \"build\"; `forge job bench` runs kind = \"run\" workflows only");
    }
    let actions_dir = dir.join("actions");
    let mut actions = BTreeMap::new();
    for p in
        toml_files(&actions_dir).with_context(|| format!("reading {}", actions_dir.display()))?
    {
        let text = std::fs::read_to_string(&p)?;
        let hash = blob_hash(repo, &p)?;
        let a = parse_action(&p, &text, hash)?;
        actions.insert(a.name.clone(), a);
    }
    let mut workflows = BTreeMap::new();
    for p in toml_files(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let text = std::fs::read_to_string(&p)?;
        let hash = blob_hash(repo, &p)?;
        let w = parse_workflow(&p, &text, hash)?;
        workflows.insert(w.name.clone(), w);
    }
    let steps = job_steps(&wf, &workflows, &actions)?;
    Ok((wf, steps))
}

/// One blob at `rev` under `dir` in `repo`: its path relative to the
/// repository root and the git blob hash `git ls-tree` already carries —
/// the same content-addressed identity `blob_hash` derives from a working
/// copy (docs/WORKFLOWS.md, "Identity is the git blob hash of the file"),
/// here read straight out of the commit with no checkout.
fn ls_tree_dir(repo: &Path, rev: &str, dir: &str) -> Result<Vec<(PathBuf, String)>> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-tree", "-r", rev, "--", dir])
        .output()?;
    if !o.status.success() {
        bail!(
            "git ls-tree {rev} -- {dir} failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let (meta, path) = l.split_once('\t')?;
            let hash = meta.split_whitespace().nth(2)?;
            Some((PathBuf::from(path), hash.to_string()))
        })
        .collect())
}

/// The bytes of one file at `rev` in `repo`, read with `git show`, no
/// checkout.
fn show_at(repo: &Path, rev: &str, path: &Path) -> Result<String> {
    let rel = path.to_str().context("path is not UTF-8")?;
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &format!("{rev}:{rel}")])
        .output()?;
    if !o.status.success() {
        bail!(
            "git show {rev}:{rel} failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Where a job's workflow was resolved from (docs/JOBS.md, "Where an
/// automation lives"): the project's own repository at its pinned commit,
/// tried first, or the operator's catalog when the repository holds no
/// workflow of that name there. Recorded on the job (`Job::workflow_source`)
/// so `forge job show` says which it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobSource {
    Repo,
    Catalog,
}

impl JobSource {
    pub fn as_str(self) -> &'static str {
        match self {
            JobSource::Repo => "repo",
            JobSource::Catalog => "catalog",
        }
    }
}

/// Every workflow under `.forge/workflows/*.toml` in a project's own
/// repository at `rev`, read with `git show` (no checkout, no working
/// tree) — for `forge workflows --project` (docs/JOBS.md, "Where an
/// automation lives"). Actions under `.forge/workflows/actions/` are not
/// themselves workflows and are skipped.
pub fn load_all_at(repo: &Path, rev: &str) -> Result<Vec<Workflow>> {
    let wf_dir = Path::new(".forge/workflows");
    let mut out = Vec::new();
    for (path, hash) in ls_tree_dir(repo, rev, wf_dir.to_str().unwrap())? {
        if path.parent() != Some(wf_dir) || path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let text = show_at(repo, rev, &path)?;
        out.push(parse_workflow(&path, &text, hash)?);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// A run workflow resolved straight from a project's own repository at a
/// pinned commit — `.forge/workflows/<name>.toml`, read with `git show`,
/// no checkout — the same content-addressed pinning the operator's
/// catalog uses (docs/JOBS.md, "Where an automation lives"). Its steps
/// resolve against the operator's own action catalog (built-ins and
/// anything the operator has added) overlaid by the project's own
/// `.forge/workflows/actions/*.toml`: an automation names a built-in
/// effect operation directly, the way docs/JOBS.md's own example does, and
/// only needs a file of its own for an action the catalog does not have.
/// Every sibling `.forge/workflows/*.toml` at the same commit (`load_all_at`)
/// is parsed too, so a step that splices in another run workflow
/// (`job_steps`) resolves against the repository's own. `None` when the
/// repository has no workflow of this name at that commit, so
/// `resolve_job_for_project` falls back to the operator's catalog
/// entirely.
pub fn resolve_job_at(
    home: &Path,
    repo: &Path,
    rev: &str,
    name: &str,
) -> Result<Option<(Workflow, Vec<RunStep>)>> {
    let wf_path = Path::new(".forge/workflows").join(format!("{name}.toml"));
    let Some((path, hash)) = ls_tree_dir(repo, rev, wf_path.to_str().unwrap())?
        .into_iter()
        .find(|(p, _)| p == &wf_path)
    else {
        return Ok(None);
    };
    let text = show_at(repo, rev, &path)?;
    let wf = parse_workflow(&path, &text, hash)?;
    if wf.kind != WorkflowKind::Run {
        bail!(
            "{name:?} is kind = \"build\" in the project's repository; `forge job start` runs kind = \"run\" workflows only"
        );
    }
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    let mut actions = cat.actions;
    for (path, hash) in ls_tree_dir(repo, rev, ".forge/workflows/actions")? {
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let text = show_at(repo, rev, &path)?;
        let a = parse_action(&path, &text, hash)?;
        actions.insert(a.name.clone(), a);
    }
    let workflows: BTreeMap<String, Workflow> = load_all_at(repo, rev)?
        .into_iter()
        .map(|w| (w.name.clone(), w))
        .collect();
    let steps = job_steps(&wf, &workflows, &actions)?;
    Ok(Some((wf, steps)))
}

/// A job's workflow, resolved the way `forge job start` does: first from
/// the project's own repository at `rev` (`resolve_job_at`), falling back
/// to the operator's catalog only when the repository has no workflow of
/// that name there (docs/JOBS.md, "Where an automation lives").
pub fn resolve_job_for_project(
    home: &Path,
    repo: &Path,
    rev: &str,
    name: &str,
) -> Result<(Workflow, Vec<RunStep>, JobSource)> {
    if let Some((wf, steps)) = resolve_job_at(home, repo, rev, name)? {
        return Ok((wf, steps, JobSource::Repo));
    }
    let (wf, steps) = resolve_job(home, name)?;
    Ok((wf, steps, JobSource::Catalog))
}

/// Every fixture under `.forge/fixtures/<workflow>/*.json` in a project's
/// own repository at `rev`, read the same way `resolve_job_at` reads the
/// workflow itself — `git show`, no checkout — for the later `forge job
/// test` (docs/JOBS.md, "Verifying an automation"). `(name, contents)`
/// pairs, sorted by name. No caller yet: `forge job test` is later build
/// order; this is the discovery primitive it will replay fixtures through.
#[allow(dead_code)]
pub fn fixtures_at(repo: &Path, rev: &str, workflow: &str) -> Result<Vec<(String, String)>> {
    let dir = Path::new(".forge/fixtures").join(workflow);
    let mut out = Vec::new();
    for (path, _hash) in ls_tree_dir(repo, rev, dir.to_str().context("path is not UTF-8")?)? {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let text = show_at(repo, rev, &path)?;
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        out.push((name, text));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// One problem `forge workflows validate` found in a repository's own
/// workflow or action file: the file, the line the parser could place it
/// at (a syntax or shape error has one; a semantic error found only after
/// a clean parse, like a step naming an unknown action, does not), and
/// the message.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidateProblem {
    pub file: PathBuf,
    pub line: Option<usize>,
    pub message: String,
}

/// What `forge workflows validate` found under a path: how many workflow
/// and action files parsed and checked out clean, and every problem
/// otherwise.
pub struct ValidateReport {
    pub workflows: usize,
    pub actions: usize,
    pub problems: Vec<ValidateProblem>,
}

/// The 1-based line a TOML deserialize error points at, read off its span
/// (present for a syntax error or an unknown/mistyped field — exactly the
/// invented-format mistakes this command exists to catch); a `bail!` from
/// this module's own semantic checks, raised after a clean parse, carries
/// no span and gets no line.
fn error_line(err: &anyhow::Error, text: &str) -> Option<usize> {
    let span = err
        .chain()
        .find_map(|e| e.downcast_ref::<toml::de::Error>())
        .and_then(|e| e.span())?;
    Some(text[..span.start.min(text.len())].matches('\n').count() + 1)
}

/// Every built-in action and operation, parsed once with no disk access:
/// the reference set a repository's own workflow steps may resolve
/// against alongside its own `.forge/workflows/actions/*.toml` (docs/JOBS.md,
/// "Where an automation lives"), without `ensure`'s side effect of
/// writing them into a home directory.
fn builtin_actions_map() -> Result<BTreeMap<String, ActionDef>> {
    let mut out = BTreeMap::new();
    for (file, text) in BUILTIN_ACTIONS.iter().chain(BUILTIN_OPERATIONS) {
        let a = parse_action(Path::new(file), text, String::new())
            .with_context(|| format!("built-in {file}"))?;
        out.insert(a.name.clone(), a);
    }
    Ok(out)
}

fn toml_files_if_present(d: &Path) -> Result<Vec<PathBuf>> {
    if !d.exists() {
        return Ok(Vec::new());
    }
    toml_files(d)
}

/// `forge workflows validate`: load every `.forge/workflows/*.toml` and
/// `.forge/workflows/actions/*.toml` under `root` with the same parsers
/// the operator's catalog uses (`parse_action`, `parse_workflow`), so a
/// file the catalog cannot load fails the same way here as it would at
/// `forge job start` — but from a plain path, with no store and no
/// FORGE2_HOME, so it runs as a repository check on any host that has the
/// `forge` binary (docs/WORKFLOWS.md, "Validating a repository's own
/// workflows"). A run workflow's steps must each resolve to a real action
/// (the repository's own or a built-in) and use `effect` on an operation
/// step only, the same rule `job_steps` enforces for `forge job start`; a
/// build workflow here only needs to parse, since a full catalog to
/// splice it against is not available offline.
pub fn validate_repo(root: &Path) -> Result<ValidateReport> {
    let wf_dir = root.join(".forge").join("workflows");
    let actions_dir = wf_dir.join("actions");

    let mut problems = Vec::new();
    let mut actions = builtin_actions_map()?;
    let mut n_actions = 0;
    for path in toml_files_if_present(&actions_dir)? {
        let text = std::fs::read_to_string(&path)?;
        let file = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        match parse_action(&path, &text, String::new()) {
            Ok(a) => {
                n_actions += 1;
                actions.insert(a.name.clone(), a);
            }
            Err(e) => problems.push(ValidateProblem {
                line: error_line(&e, &text),
                file,
                message: format!("{e:#}"),
            }),
        }
    }

    // Parsed first, all of them, so a run workflow's step that splices in a
    // sibling run workflow (`job_steps`) resolves against every workflow
    // this repository declares, not just the one named on the command line.
    let mut n_workflows = 0;
    let mut workflows: BTreeMap<String, Workflow> = BTreeMap::new();
    let mut parsed: Vec<(PathBuf, String, Workflow)> = Vec::new();
    for path in toml_files_if_present(&wf_dir)? {
        let text = std::fs::read_to_string(&path)?;
        let file = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        match parse_workflow(&path, &text, String::new()) {
            Ok(wf) => {
                workflows.insert(wf.name.clone(), wf.clone());
                parsed.push((file, text, wf));
            }
            Err(e) => problems.push(ValidateProblem {
                line: error_line(&e, &text),
                file,
                message: format!("{e:#}"),
            }),
        }
    }
    for (file, text, wf) in &parsed {
        let result = if wf.kind == WorkflowKind::Run {
            job_steps(wf, &workflows, &actions).map(|_| ())
        } else {
            Ok(())
        };
        match result {
            Ok(()) => n_workflows += 1,
            Err(e) => problems.push(ValidateProblem {
                line: error_line(&e, text),
                file: file.clone(),
                message: format!("{e:#}"),
            }),
        }
    }

    Ok(ValidateReport {
        workflows: n_workflows,
        actions: n_actions,
        problems,
    })
}

/// One thing wrong with a file, and whether it blocks use.
#[derive(Debug, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    pub blocking: bool,
    pub what: String,
}

/// Every file, checked structurally; every workflow, resolved. Parse
/// errors are problems rather than errors, so one bad file does not hide
/// the rest. Deterministic: same files, same list.
pub fn check(home: &Path) -> Result<Vec<Problem>> {
    let Catalog {
        workflows,
        actions,
        mut problems,
    } = load_catalog(home)?;
    for (name, a) in &actions {
        if a.kind == Kind::Operation && KERNEL_OPS.contains(&name.as_str()) {
            problems.push(Problem {
                file: format!("actions/{name}.toml"),
                blocking: true,
                what: format!("operation {name:?} shadows a kernel operation"),
            });
        }
    }
    for (name, wf) in &workflows {
        // A run workflow does not follow the build data-flow rules
        // (`check_flow` requires a directive, which an operation-only job
        // never has); it only needs its steps' actions (and any spliced-in
        // sibling run workflow's) to exist (see `job_steps`).
        let r = if wf.kind == WorkflowKind::Run {
            job_steps(wf, &workflows, &actions).map(|_| ())
        } else {
            let mut out = Resolved::default();
            splice(wf, &workflows, &actions, &mut Vec::new(), &mut out)
                .and_then(|_| check_flow(&out.steps))
        };
        if let Err(e) = r {
            problems.push(Problem {
                file: format!("{name}.toml"),
                blocking: true,
                what: format!("{e:#}"),
            });
        }
    }
    Ok(problems)
}

/// Files changed since the last commit of the directory, or all files if
/// it has never been committed.
pub fn uncommitted(home: &Path) -> Result<Vec<String>> {
    let dir = ensure(home)?;
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()?;
    Ok(crate::git::porcelain_paths(&String::from_utf8_lossy(
        &o.stdout,
    )))
}

/// The commit that introduced a blob into the directory's history, if it
/// has been committed. For "which commit do I check out to revert".
pub fn commit_for(home: &Path, hash: &str) -> Option<String> {
    let dir = dir_of(home);
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["log", "--format=%h %cs", "--find-object", hash, "--reverse"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .next()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, rel: &str, text: &str) {
        std::fs::write(home.join("workflows").join(rel), text).unwrap();
    }

    #[test]
    fn contracts_and_products_serialize_by_their_file_names() {
        // The stored resolved JSON and the action files carry these names;
        // the enums must round-trip them byte for byte.
        for c in Contract::ALL {
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{}\"", c.as_str()));
            assert_eq!(serde_json::from_str::<Contract>(&json).unwrap(), c);
            assert_eq!(Contract::parse(c.as_str()), Some(c));
        }
        for p in Product::ALL {
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
            assert_eq!(serde_json::from_str::<Product>(&json).unwrap(), p);
            assert_eq!(Product::parse(p.as_str()), Some(p));
        }
        assert_eq!(Product::VerifyRef.as_str(), "verify_ref");
        assert!(Contract::parse("verify").is_none());
        assert!(Product::parse("tests").is_none());
        assert!(Contract::Code.writes() && Contract::Tests.writes());
        assert!(!Contract::Review.writes() && !Contract::Plan.writes());
    }

    #[test]
    fn every_built_in_parses_on_its_own_and_names_are_unique() {
        // A built-in no workflow references would otherwise fail only at
        // first load in production; and a duplicate entry is written once
        // and never noticed. Actions and operations share a directory;
        // workflows have their own, so `docs` may be both.
        let dir = tempfile::tempdir().unwrap();
        let mut actions = std::collections::HashSet::new();
        for (file, text) in BUILTIN_ACTIONS.iter().chain(BUILTIN_OPERATIONS) {
            std::fs::write(dir.path().join(file), text).unwrap();
            let hash = blob_hash(dir.path(), &dir.path().join(file)).unwrap();
            let a = parse_action(&dir.path().join(file), text, hash)
                .unwrap_or_else(|e| panic!("{file}: {e:#}"));
            assert!(
                actions.insert(a.name.clone()),
                "{file}: duplicate built-in action {}",
                a.name
            );
        }
        let mut workflows = std::collections::HashSet::new();
        for (file, text) in BUILTIN_WORKFLOWS {
            std::fs::write(dir.path().join(file), text).unwrap();
            let hash = blob_hash(dir.path(), &dir.path().join(file)).unwrap();
            let w = parse_workflow(&dir.path().join(file), text, hash)
                .unwrap_or_else(|e| panic!("{file}: {e:#}"));
            assert!(
                workflows.insert(w.name.clone()),
                "{file}: duplicate built-in workflow {}",
                w.name
            );
        }
    }

    #[test]
    fn builtins_resolve_and_carry_blob_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let all = load_all(dir.path()).unwrap();
        assert_eq!(
            all.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec![
                "cheap",
                "concierge",
                "direct",
                "docs",
                "documented",
                "intake",
                "mapped",
                "planned",
                "playable",
                "polish",
                "reviewed",
                "tdd",
                "tdd-reviewed"
            ]
        );
        for w in &all {
            resolve(dir.path(), &w.name).unwrap_or_else(|e| panic!("{}: {e:#}", w.name));
        }
        let r = resolve(dir.path(), "tdd").unwrap();
        assert_eq!(
            r.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["tests", "setup", "repo-map", "code"]
        );
        assert_eq!(
            r.steps[0].max_turns,
            Some(40),
            "the action's own default applies"
        );
        assert_eq!(r.steps[1].action.kind, Kind::Operation);
        assert_eq!(r.pins.len(), 5, "the workflow and four actions");
        let rr = resolve(dir.path(), "tdd-reviewed").unwrap();
        assert_eq!(
            rr.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["tests", "setup", "repo-map", "code", "review"]
        );
        assert!(r.pins.iter().all(|p| p.hash.len() == 40), "git blob hashes");
        assert!(check(dir.path()).unwrap().iter().all(|p| !p.blocking));
        // Editing a file changes only its own version.
        let before = resolve(dir.path(), "direct").unwrap();
        write(
            dir.path(),
            "actions/code.toml",
            "name = \"code\"\nkind = \"directive\"\ndescription = \"x\"\nconsumes = [\"branch\"]\nproduces = [\"branch\"]\nmax_turns = 50\n",
        );
        let after = resolve(dir.path(), "direct").unwrap();
        assert_ne!(before.pins, after.pins);
        assert_eq!(
            after.steps[2].max_turns,
            Some(50),
            "code is now the third step, after repo-map"
        );
        assert_eq!(
            before.pins.iter().find(|p| p.name == "setup"),
            after.pins.iter().find(|p| p.name == "setup"),
            "setup is unchanged"
        );
    }

    #[test]
    fn inline_composition_splices_and_rejects_cycles() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "outer.toml",
            "name = \"outer\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { workflow = \"tdd\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
        );
        let r = resolve(dir.path(), "outer").unwrap();
        assert_eq!(
            r.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["setup", "tests", "setup", "repo-map", "code"]
        );
        assert_eq!(r.steps[1].via, vec!["outer", "tdd"]);
        assert!(
            r.pins
                .iter()
                .any(|p| p.kind == "workflow" && p.name == "tdd")
        );
        write(
            dir.path(),
            "a.toml",
            "name = \"a\"\nsteps = [{ workflow = \"b\" }]\n",
        );
        write(
            dir.path(),
            "b.toml",
            "name = \"b\"\nsteps = [{ workflow = \"a\" }]\n",
        );
        let err = resolve(dir.path(), "a").unwrap_err().to_string();
        assert!(err.contains("references itself"), "{err}");
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.what.contains("references itself")),
            "{problems:?}"
        );
    }

    #[test]
    fn operations_produce_only_branch_or_interface_and_a_mutating_one_yields_a_verdict() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        for (name, body, want) in [
            (
                "verdicting",
                "produces = [\"verdict\"]\nrun = [\"true\"]\n",
                "cannot produce \"verdict\"",
            ),
            (
                "modelled",
                "model = \"haiku\"\nrun = [\"true\"]\n",
                "`model` applies to directives only",
            ),
        ] {
            write(
                dir.path(),
                &format!("actions/{name}.toml"),
                &format!(
                    "name = \"{name}\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\n{body}"
                ),
            );
            let err = load_actions(dir.path()).unwrap_err().to_string();
            assert!(err.contains(want), "{name}: {err}");
            std::fs::remove_file(
                dir.path()
                    .join("workflows/actions")
                    .join(format!("{name}.toml")),
            )
            .unwrap();
        }
        // The built-in fmt mutates, so polish (which consumes a verdict)
        // may follow it directly; the built-in interface reads the verify
        // ref and so needs the tests directive first.
        let fmt = load_actions(dir.path()).unwrap().remove("fmt").unwrap();
        assert!(fmt.mutates() && !fmt.yields_interface() && !fmt.reads_verify_ref());
        write(
            dir.path(),
            "fmt-polish.toml",
            "name = \"fmt-polish\"\nsteps = [{ action = \"fmt\" }, { action = \"polish\" }]\n",
        );
        assert!(resolve(dir.path(), "fmt-polish").is_ok());
        write(
            dir.path(),
            "iface-early.toml",
            "name = \"iface-early\"\nsteps = [{ action = \"interface\" }, { action = \"code\" }]\n",
        );
        let err = resolve(dir.path(), "iface-early").unwrap_err().to_string();
        assert!(err.contains("consumes \"verify_ref\""), "{err}");
        write(
            dir.path(),
            "iface.toml",
            "name = \"iface\"\nsteps = [{ action = \"tests\" }, { action = \"interface\" }, { action = \"code\" }]\n",
        );
        let r = resolve(dir.path(), "iface").unwrap();
        assert!(r.steps[1].action.reads_verify_ref() && r.steps[1].action.yields_interface());
    }

    #[test]
    fn data_flow_and_kernel_shadowing_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "actions/needs-ref.toml",
            "name = \"needs-ref\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"verify_ref\"]\nrun = [\"true\"]\n",
        );
        write(
            dir.path(),
            "early.toml",
            "name = \"early\"\nsteps = [{ action = \"needs-ref\" }, { action = \"code\" }]\n",
        );
        let err = resolve(dir.path(), "early").unwrap_err().to_string();
        assert!(err.contains("consumes \"verify_ref\""), "{err}");
        write(
            dir.path(),
            "late.toml",
            "name = \"late\"\nsteps = [{ action = \"tests\" }, { action = \"needs-ref\" }, { action = \"code\" }]\n",
        );
        assert!(resolve(dir.path(), "late").is_ok());
        write(
            dir.path(),
            "opsonly.toml",
            "name = \"opsonly\"\nsteps = [{ action = \"setup\" }]\n",
        );
        assert!(
            resolve(dir.path(), "opsonly")
                .unwrap_err()
                .to_string()
                .contains("at least one directive")
        );
        write(
            dir.path(),
            "actions/verify.toml",
            "name = \"verify\"\nkind = \"operation\"\ndescription = \"d\"\nrun = [\"true\"]\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.what.contains("shadows a kernel operation")),
            "{problems:?}"
        );
        write(
            dir.path(),
            "actions/both.toml",
            "name = \"both\"\nkind = \"operation\"\nrun = [\"true\"]\ncheck = \"test\"\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems.iter().any(|p| p.what.contains("exactly one of")),
            "{problems:?}"
        );
    }

    #[test]
    fn directives_name_a_kernel_contract() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "actions/audit.toml",
            "name = \"audit\"\nkind = \"directive\"\ndescription = \"d\"\nconsumes = [\"branch\"]\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.what.contains("not one the kernel enforces")),
            "{problems:?}"
        );
        std::fs::remove_file(dir.path().join("workflows/actions/audit.toml")).unwrap();
        write(
            dir.path(),
            "actions/tidy.toml",
            "name = \"tidy\"\nkind = \"directive\"\ncontract = \"code\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nproduces = [\"branch\"]\npaths = [\"src/\"]\nbrief = \"only tidy\"\n",
        );
        let a = load_actions(dir.path()).unwrap();
        assert_eq!(a["tidy"].contract, Contract::Code);
        assert_eq!(a["docs"].paths, vec!["docs/", "*.md"]);
        assert_eq!(a["polish"].contract, Contract::Code);
        assert!(!a["polish"].brief.is_empty());
        write(
            dir.path(),
            "actions/badpaths.toml",
            "name = \"badpaths\"\nkind = \"directive\"\ncontract = \"review\"\npaths = [\"x\"]\n",
        );
        assert!(
            check(dir.path())
                .unwrap()
                .iter()
                .any(|p| p.what.contains("code contract only"))
        );
    }

    #[test]
    fn typos_are_blocking_and_overrides_have_a_precedence() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "actions/typo.toml",
            "name = \"typo\"\nkind = \"operation\"\ndescription = \"d\"\nrun = [\"true\"]\ntimeout_sec = 5\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems.iter().any(|p| p.blocking
                && p.file == "actions/typo.toml"
                && p.what.contains("unknown field")),
            "{problems:?}"
        );
        write(
            dir.path(),
            "wtypo.toml",
            "name = \"wtypo\"\nsteps = [{ action = \"code\", max_turn = 3 }]\n[meta]\nuse_wen = \"x\"\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.file == "wtypo.toml"),
            "{problems:?}"
        );
        std::fs::remove_file(dir.path().join("workflows/actions/typo.toml")).unwrap();
        std::fs::remove_file(dir.path().join("workflows/wtypo.toml")).unwrap();
        // step override > action default > task default (None here)
        write(
            dir.path(),
            "over.toml",
            "name = \"over\"\nsteps = [{ action = \"tests\", max_turns = 9, model = \"haiku\" }, { action = \"code\" }]\n",
        );
        let r = resolve(dir.path(), "over").unwrap();
        assert_eq!(r.steps[0].max_turns, Some(9));
        assert_eq!(r.steps[0].model.as_deref(), Some("haiku"));
        assert_eq!(r.steps[1].max_turns, None, "the task's own limit applies");
        // diamond: two paths to the same child splice twice, pin once
        write(
            dir.path(),
            "left.toml",
            "name = \"left\"\nsteps = [{ workflow = \"direct\" }]\n",
        );
        write(
            dir.path(),
            "right.toml",
            "name = \"right\"\nsteps = [{ workflow = \"direct\" }]\n",
        );
        write(
            dir.path(),
            "diamond.toml",
            "name = \"diamond\"\nsteps = [{ workflow = \"left\" }, { workflow = \"right\" }]\n",
        );
        let r = resolve(dir.path(), "diamond").unwrap();
        assert_eq!(
            r.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["setup", "repo-map", "code", "setup", "repo-map", "code"]
        );
        assert_eq!(r.pins.iter().filter(|p| p.name == "direct").count(), 1);
        assert_eq!(r.pins.iter().filter(|p| p.name == "code").count(), 1);
    }

    #[test]
    fn verifying_operations_need_a_preceding_directive() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        let a = load_actions(dir.path()).unwrap();
        assert!(a["playwright"].overlay && a["playwright"].verifies);
        write(
            dir.path(),
            "early.toml",
            "name = \"early\"\nsteps = [{ action = \"playwright\" }, { action = \"code\" }]\n",
        );
        let err = resolve(dir.path(), "early").unwrap_err().to_string();
        assert!(err.contains("no directive precedes it"), "{err}");
        write(
            dir.path(),
            "actions/badd.toml",
            "name = \"badd\"\nkind = \"directive\"\ncontract = \"code\"\noverlay = true\n",
        );
        assert!(
            check(dir.path())
                .unwrap()
                .iter()
                .any(|p| p.what.contains("operations only"))
        );
    }

    #[test]
    fn old_kind_spelling_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "old.toml",
            "name = \"old\"\nsteps = [{ kind = \"code\" }]\n",
        );
        assert_eq!(
            resolve(dir.path(), "old").unwrap().steps[0].action.name,
            "code"
        );
    }

    /// docs/JOBS.md, "The definition", reordered so `steps` sits at the
    /// top level: TOML binds a bare `key = value` to the nearest
    /// preceding table header, so `steps` must come before `[trigger]`
    /// to be the workflow's own field rather than `trigger.steps`.
    const JOBS_MD_EXAMPLE: &str = r#"
name = "quote-by-text"
kind = "run"
description = "a customer texts a photo of a job; they get a quote back and it goes in the book"

steps = [
  { action = "extract-job",  role = "read" },
  { action = "price-job" },
  { action = "draft-quote",  role = "write" },
  { action = "send-quote",   effect = "message" },
  { action = "log-quote",    effect = "row" },
]

[trigger]
on = "message"
contact = "customers"

[assert]
quoted  = ["scripts/assert-quote.sh"]

[skip_if]
already_quoted = ["scripts/skip-if-already-quoted.sh"]

[limits]
budget_usd = 0.10
per_day    = 200
on_failure = "ask:contact"
"#;

    #[test]
    fn a_run_workflow_parses_the_jobs_md_example() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(dir.path(), "quote-by-text.toml", JOBS_MD_EXAMPLE);
        let w = get(dir.path(), "quote-by-text").unwrap().unwrap();
        assert_eq!(w.kind, WorkflowKind::Run);
        let t = w.trigger.as_ref().unwrap();
        assert_eq!(t.on, TriggerOn::Message);
        assert_eq!(t.value(), Some("customers"));
        assert_eq!(
            w.assert.get("quoted").unwrap(),
            &vec!["scripts/assert-quote.sh".to_string()]
        );
        assert_eq!(
            w.skip_if.get("already_quoted").unwrap(),
            &vec!["scripts/skip-if-already-quoted.sh".to_string()]
        );
        let limits = w.limits.as_ref().unwrap();
        assert_eq!(limits.budget_usd, 0.10);
        assert_eq!(limits.per_day, 200);
        assert_eq!(limits.on_failure, OnFailure::AskContact);
        assert_eq!(w.steps[0].role.as_deref(), Some("read"));
        assert_eq!(w.steps[3].effect, Some(EffectKind::Message));
        assert_eq!(w.steps[4].effect, Some(EffectKind::Row));
    }

    #[test]
    fn a_run_workflow_env_parses_into_a_map() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "thresholds.toml",
            "name = \"thresholds\"\nsteps = [{ action = \"code\", effect = \"row\" }]\nkind = \"run\"\n[trigger]\non = \"manual\"\n[env]\nMAX_LINES = \"400\"\nOTHER = \"3000\"\n",
        );
        let w = get(dir.path(), "thresholds").unwrap().unwrap();
        assert_eq!(w.env.get("MAX_LINES").unwrap(), "400");
        assert_eq!(w.env.get("OTHER").unwrap(), "3000");
    }

    #[test]
    fn a_build_workflow_may_not_have_run_sections() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "wrongly-run.toml",
            "name = \"wrongly-run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"manual\"\n",
        );
        let err = get(dir.path(), "wrongly-run").unwrap_err().to_string();
        assert!(err.contains("may not have [trigger]"), "{err}");
        write(
            dir.path(),
            "wrongly-run.toml",
            "name = \"wrongly-run\"\nsteps = [{ action = \"code\" }]\n[limits]\nbudget_usd = 1.0\nper_day = 1\non_failure = \"drop\"\n",
        );
        let err = get(dir.path(), "wrongly-run").unwrap_err().to_string();
        assert!(err.contains("may not have [limits]"), "{err}");
        write(
            dir.path(),
            "wrongly-run.toml",
            "name = \"wrongly-run\"\nsteps = [{ action = \"code\" }]\n[skip_if]\nalready_done = [\"true\"]\n",
        );
        let err = get(dir.path(), "wrongly-run").unwrap_err().to_string();
        assert!(err.contains("may not have [skip_if]"), "{err}");
        write(
            dir.path(),
            "wrongly-run.toml",
            "name = \"wrongly-run\"\nsteps = [{ action = \"code\" }]\n[env]\nMAX = \"1\"\n",
        );
        let err = get(dir.path(), "wrongly-run").unwrap_err().to_string();
        assert!(err.contains("may not have [env]"), "{err}");
    }

    #[test]
    fn an_unknown_trigger_on_is_refused_with_the_file_and_line() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "carrier-pigeon.toml",
            "name = \"carrier-pigeon\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"carrier-pigeon\"\n",
        );
        let err = get(dir.path(), "carrier-pigeon").unwrap_err().to_string();
        assert!(err.contains("carrier-pigeon.toml"), "{err}");
        assert!(err.contains("line"), "{err}");
        assert!(err.contains("unknown variant"), "{err}");
    }

    #[test]
    fn an_invalid_cron_is_refused_at_parse_time_with_the_file_and_line() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "off-the-rails.toml",
            "name = \"off-the-rails\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"schedule\"\ncron = \"not a cron\"\n",
        );
        let err = get(dir.path(), "off-the-rails").unwrap_err().to_string();
        assert!(err.contains("off-the-rails.toml"), "{err}");
        assert!(err.contains("line"), "{err}");
        assert!(err.contains("invalid cron"), "{err}");
    }

    #[test]
    fn parse_duration_reads_s_m_h_d_and_rejects_junk() {
        assert_eq!(parse_duration("0s"), Ok(0));
        assert_eq!(parse_duration("5s"), Ok(5));
        assert_eq!(parse_duration("5m"), Ok(300));
        assert_eq!(parse_duration("1h"), Ok(3600));
        assert_eq!(parse_duration("2d"), Ok(172_800));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("m").is_err());
        assert!(parse_duration("5mins").is_err());
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("-5m").is_err());
    }

    #[test]
    fn a_trigger_delay_parses_into_seconds() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "quote-later.toml",
            "name = \"quote-later\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"manual\"\ndelay = \"5m\"\n",
        );
        let w = get(dir.path(), "quote-later").unwrap().unwrap();
        assert_eq!(w.trigger.as_ref().unwrap().delay, Some(300));
    }

    #[test]
    fn an_invalid_trigger_delay_is_refused_at_parse_time_with_the_file_and_line() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "quote-never.toml",
            "name = \"quote-never\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"manual\"\ndelay = \"soon\"\n",
        );
        let err = get(dir.path(), "quote-never").unwrap_err().to_string();
        assert!(err.contains("quote-never.toml"), "{err}");
        assert!(err.contains("line"), "{err}");
        assert!(err.contains("invalid duration"), "{err}");
    }

    #[test]
    fn a_run_workflow_needs_a_trigger() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "untriggered.toml",
            "name = \"untriggered\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n",
        );
        let err = get(dir.path(), "untriggered").unwrap_err().to_string();
        assert!(err.contains("kind = \"run\" needs a [trigger]"), "{err}");
    }

    /// A run workflow's step may name a sibling run workflow instead of an
    /// action, spliced inline recursively (`job_steps`) — the composition
    /// `doctor-daily.toml` uses to pull in `disk-and-logs.toml`. Splicing a
    /// `kind = "build"` workflow, or a cycle of run workflows, is refused.
    #[test]
    fn resolve_job_splices_a_sibling_run_workflow_and_rejects_a_build_kind_child_or_a_cycle() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "inner-run.toml",
            "name = \"inner-run\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ action = \"fmt\" }]\n[trigger]\non = \"manual\"\n",
        );
        write(
            dir.path(),
            "outer-run.toml",
            "name = \"outer-run\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ action = \"fmt\" }, { workflow = \"inner-run\" }]\n[trigger]\non = \"manual\"\n",
        );
        let (wf, steps) = resolve_job(dir.path(), "outer-run").unwrap();
        assert_eq!(wf.kind, WorkflowKind::Run);
        assert_eq!(
            steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["fmt", "fmt"]
        );

        write(
            dir.path(),
            "build-child.toml",
            "name = \"build-child\"\ndescription = \"d\"\nsteps = [{ action = \"code\" }]\n",
        );
        write(
            dir.path(),
            "bad-splice.toml",
            "name = \"bad-splice\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ workflow = \"build-child\" }]\n[trigger]\non = \"manual\"\n",
        );
        let err = resolve_job(dir.path(), "bad-splice")
            .unwrap_err()
            .to_string();
        assert!(err.contains("kind = \"build\""), "{err}");

        write(
            dir.path(),
            "cycle-a.toml",
            "name = \"cycle-a\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ workflow = \"cycle-b\" }]\n[trigger]\non = \"manual\"\n",
        );
        write(
            dir.path(),
            "cycle-b.toml",
            "name = \"cycle-b\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ workflow = \"cycle-a\" }]\n[trigger]\non = \"manual\"\n",
        );
        let err = resolve_job(dir.path(), "cycle-a").unwrap_err().to_string();
        assert!(err.contains("references itself"), "{err}");
    }

    #[test]
    fn on_failure_round_trips_and_rejects_junk() {
        for (s, want) in [
            ("ask:contact", OnFailure::AskContact),
            ("ask:operator", OnFailure::AskOperator),
            ("retry:3", OnFailure::Retry(3)),
            ("drop", OnFailure::Drop),
        ] {
            assert_eq!(OnFailure::parse(s), Some(want.clone()));
            assert_eq!(want.as_str(), s);
        }
        assert_eq!(OnFailure::parse("retry:"), None);
        assert_eq!(OnFailure::parse("retry:x"), None);
        assert_eq!(OnFailure::parse("ask"), None);
        for on in [
            TriggerOn::Manual,
            TriggerOn::Schedule,
            TriggerOn::Message,
            TriggerOn::Webhook,
            TriggerOn::Event,
        ] {
            let json = serde_json::to_string(&on).unwrap();
            assert_eq!(json, format!("\"{}\"", on.as_str()));
        }
        for e in [
            EffectKind::Message,
            EffectKind::Row,
            EffectKind::File,
            EffectKind::Http,
        ] {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(json, format!("\"{}\"", e.as_str()));
        }
    }

    #[test]
    fn the_docs_name_every_built_in() {
        // Agents read docs/ACTIONS.md as instructions; a mechanism the docs
        // do not name might as well not exist, and one they name that does
        // not exist is worse.
        let docs = include_str!("../docs/ACTIONS.md");
        for (file, text) in BUILTIN_ACTIONS
            .iter()
            .chain(BUILTIN_OPERATIONS)
            .chain(BUILTIN_WORKFLOWS)
        {
            let name = text
                .lines()
                .find_map(|l| l.strip_prefix("name = "))
                .map(|n| n.trim_matches('"'))
                .unwrap_or_else(|| panic!("{file} has no name"));
            assert!(
                docs.contains(&format!("`{name}`")),
                "docs/ACTIONS.md does not mention `{name}` ({file})"
            );
        }

        // The built-in workflows table must list each workflow's steps in
        // the exact order the kernel resolves them, compositions spliced
        // inline: a table that lied about the order would mislead whoever
        // picks a workflow by reading it.
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        for (file, _) in BUILTIN_WORKFLOWS {
            let name = file.strip_suffix(".toml").unwrap();
            let resolved = resolve(dir.path(), name).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let steps: Vec<&str> = resolved
                .steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect();
            let row = docs
                .lines()
                .find(|l| l.starts_with(&format!("| `{name}` |")))
                .unwrap_or_else(|| {
                    panic!("docs/ACTIONS.md has no built-in workflows row for `{name}`")
                });
            let cell = row
                .split('|')
                .nth(2)
                .unwrap_or_else(|| panic!("malformed built-in workflows row for `{name}`: {row}"));
            let listed: Vec<&str> = cell
                .split('→')
                .map(|s| s.split('(').next().unwrap().trim())
                .collect();
            assert_eq!(
                listed, steps,
                "docs/ACTIONS.md built-in workflows row for `{name}` does not match how it resolves"
            );
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    #[test]
    fn resolve_job_at_reads_a_run_workflow_from_the_repository_at_a_pinned_commit_merging_operator_and_project_actions()
     {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        git(r, &["init", "-q", "-b", "main"]);
        git(r, &["config", "user.email", "t@example.com"]);
        git(r, &["config", "user.name", "t"]);
        std::fs::create_dir_all(r.join(".forge/workflows/actions")).unwrap();
        std::fs::create_dir_all(r.join(".forge/fixtures/publish-snapshot")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            r#"name = "publish-snapshot"
kind = "run"
description = "writes a file with a built-in operation and logs with the project's own"

steps = [
  { action = "write-file", effect = "file" },
  { action = "custom-op",  effect = "row" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]
"#,
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/workflows/actions/custom-op.toml"),
            "name = \"custom-op\"\nkind = \"operation\"\ndescription = \"the project's own operation\"\nrun = [\"true\"]\n",
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/fixtures/publish-snapshot/01-example.json"),
            "{\"a\":1}\n",
        )
        .unwrap();
        git(r, &["add", "-A"]);
        git(r, &["commit", "-q", "-m", "automation"]);
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&o.stdout).trim().to_string();

        let home = tempfile::tempdir().unwrap();
        load_all(home.path()).unwrap();

        let (wf, steps) = resolve_job_at(home.path(), r, &sha, "publish-snapshot")
            .unwrap()
            .unwrap();
        assert_eq!(wf.kind, WorkflowKind::Run);
        let names: Vec<&str> = steps.iter().map(|s| s.action.name.as_str()).collect();
        assert_eq!(names, vec!["write-file", "custom-op"]);

        // A name the repository does not have at that commit: no fallback
        // to the catalog inside `resolve_job_at` itself.
        assert!(
            resolve_job_at(home.path(), r, &sha, "no-such-workflow")
                .unwrap()
                .is_none()
        );

        let (_, _, source) =
            resolve_job_for_project(home.path(), r, &sha, "publish-snapshot").unwrap();
        assert_eq!(source, JobSource::Repo);
        // Falls to the operator's catalog when the repository has none by
        // that name.
        write(
            home.path(),
            "catalog-only.toml",
            r#"name = "catalog-only"
kind = "run"
description = "a run workflow the operator's catalog carries but the repository does not"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]
"#,
        );
        let (_, _, source) = resolve_job_for_project(home.path(), r, &sha, "catalog-only").unwrap();
        assert_eq!(source, JobSource::Catalog);

        let all = load_all_at(r, &sha).unwrap();
        assert_eq!(
            all.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec!["publish-snapshot"]
        );

        let fixtures = fixtures_at(r, &sha, "publish-snapshot").unwrap();
        assert_eq!(fixtures.len(), 1);
        assert_eq!(fixtures[0].0, "01-example");
        assert!(fixtures[0].1.contains("\"a\""));
    }

    #[test]
    fn validate_repo_accepts_a_clean_tree_with_no_store_and_no_forge2_home() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows/actions")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            r#"name = "publish-snapshot"
kind = "run"
description = "writes a file with a built-in operation and logs with the project's own"

steps = [
  { action = "write-file", effect = "file" },
  { action = "custom-op",  effect = "row" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#,
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/workflows/actions/custom-op.toml"),
            "name = \"custom-op\"\nkind = \"operation\"\ndescription = \"the project's own operation\"\nrun = [\"true\"]\n",
        )
        .unwrap();

        // Reading FORGE2_HOME here would panic the test process (it does not
        // exist); `validate_repo` takes only `r`, never a home.
        let report = validate_repo(r).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.workflows, 1);
        assert_eq!(report.actions, 1);
    }

    /// `doctor-daily.toml` splicing in `disk-and-logs.toml` (docs/JOBS.md,
    /// "Steps"): a run workflow's step may name a sibling run workflow
    /// instead of an action, and `forge workflows validate` resolves it
    /// against every workflow the repository declares, not just the one
    /// named.
    #[test]
    fn validate_repo_accepts_a_run_workflow_splicing_a_sibling_run_workflow() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/outer.toml"),
            "name = \"outer\"\nkind = \"run\"\ndescription = \"d\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n  { workflow = \"inner\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/workflows/inner.toml"),
            "name = \"inner\"\nkind = \"run\"\ndescription = \"d\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.workflows, 2);
    }

    #[test]
    fn validate_repo_accepts_an_empty_or_absent_forge_directory() {
        let repo = tempfile::tempdir().unwrap();
        let report = validate_repo(repo.path()).unwrap();
        assert!(report.problems.is_empty());
        assert_eq!(report.workflows, 0);
        assert_eq!(report.actions, 0);
    }

    /// The three invented shapes equitizr's `.forge/workflows/publish-snapshot.toml`
    /// landed twice, that a repository's own checks never validated
    /// because nothing ran the catalog's loader against it.
    #[test]
    fn validate_repo_catches_a_string_trigger() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"invented string trigger\"\ntrigger = \"manual\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n]\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(
            p.file,
            PathBuf::from(".forge/workflows/publish-snapshot.toml")
        );
        assert_eq!(p.line, Some(4), "{}", p.message);
        assert!(p.message.contains("invalid type"), "{}", p.message);
    }

    #[test]
    fn validate_repo_catches_a_steps_table_with_an_inline_run_command() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"invented [[steps]] table with an inline run command\"\n\n[[steps]]\nrun = \"echo hi\"\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(p.line, Some(6), "{}", p.message);
        assert!(p.message.contains("unknown field `run`"), "{}", p.message);
    }

    #[test]
    fn validate_repo_catches_an_http_get_step_with_url_and_field() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"invented http_get step with url and field\"\n\nsteps = [\n  { action = \"http_get\", url = \"https://example.com\", field = \"x\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(p.line, Some(6), "{}", p.message);
        assert!(
            p.message.contains("unknown field `url`")
                || p.message.contains("unknown field `field`"),
            "{}",
            p.message
        );
    }

    #[test]
    fn validate_repo_catches_a_step_referencing_an_unknown_action() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"references an action nothing declares\"\n\nsteps = [\n  { action = \"does-not-exist\", effect = \"file\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(
            p.line, None,
            "a semantic check after a clean parse has no span"
        );
        assert!(p.message.contains("does-not-exist"), "{}", p.message);
    }
}
