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
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
}

impl Trigger {
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
    #[serde(default)]
    cron: Option<String>,
    #[serde(default)]
    contact: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
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
    /// `kind = "run"` only: budget and failure policy.
    limits: Option<Limits>,
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
    /// `kind = "run"` only.
    pub limits: Option<Limits>,
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

fn parse_action(dir: &Path, path: &Path, text: &str) -> Result<ActionDef> {
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
            || raw.file_into_initiative)
    {
        bail!(
            "{}: contract, paths, brief, prompt, and file_into_initiative apply to directives only",
            path.display()
        );
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
        file_into_initiative: raw.file_into_initiative,
        overlay: raw.overlay,
        verifies: raw.verifies,
        output: raw.output,
        hash: blob_hash(dir, path)?,
        text: text.to_string(),
    })
}

fn parse_workflow(dir: &Path, path: &Path, text: &str) -> Result<Workflow> {
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
            if raw.limits.is_some() {
                bail!(
                    "{}: kind = \"build\" (the default) may not have [limits]; that is a run workflow's section (set kind = \"run\")",
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
        limits: raw.limits,
        hash: blob_hash(dir, path)?,
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
        |p, t| parse_action(&dir, p, t),
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
        |p, t| parse_workflow(&dir, p, t),
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
        let mut out = Resolved::default();
        let r = splice(wf, &workflows, &actions, &mut Vec::new(), &mut out)
            .and_then(|_| check_flow(&out.steps));
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
            let a = parse_action(dir.path(), &dir.path().join(file), text)
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
            let w = parse_workflow(dir.path(), &dir.path().join(file), text)
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
        let limits = w.limits.as_ref().unwrap();
        assert_eq!(limits.budget_usd, 0.10);
        assert_eq!(limits.per_day, 200);
        assert_eq!(limits.on_failure, OnFailure::AskContact);
        assert_eq!(w.steps[0].role.as_deref(), Some("read"));
        assert_eq!(w.steps[3].effect, Some(EffectKind::Message));
        assert_eq!(w.steps[4].effect, Some(EffectKind::Row));
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
}
