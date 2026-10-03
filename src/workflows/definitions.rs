//! One action file, one version ([`ActionDef`]); one workflow file, one
//! version ([`Workflow`]): the shapes a catalog file parses into, and the
//! parsers themselves (docs/ACTIONS.md, docs/WORKFLOWS.md).

use super::*;

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
    /// Operation: the args a deploy target running it must give, non-empty.
    #[serde(default)]
    required_args: Vec<String>,
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
    /// Directive: the prompt lives in this file beside the action, in the
    /// catalog, and may include fragments (`{{> name}}`). Instead of
    /// `prompt`, not with it.
    prompt_file: Option<String>,
    /// Directive: the JSON Schema a job step's structured output is
    /// validated against before the next step sees it (docs/JOBS.md,
    /// "Steps"). Unused by a build workflow, which holds every directive to
    /// the envelope schema instead.
    schema: Option<String>,
    /// Directive: the named outcomes its structured result picks one of
    /// (docs/EXECUTION.md, "Outcomes, then edges").
    /// A table of `outcome = "description"` also serves: the descriptions are
    /// the criteria a `jev` provider judges among; the list form names each
    /// outcome as its own description.
    outcomes: Option<judgment::OutcomesRaw>,
    /// Directive (jev runner): further typed questions beside the outcomes.
    #[serde(default)]
    questions: Vec<Question>,
    /// Directive (jev runner): `{ 0.6 = "uncertain" }` routes a judgment whose
    /// confidence is below 0.6 to the outcome `uncertain`.
    confidence_below: Option<serde_json::Value>,
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
    #[serde(default)]
    pub required_args: Vec<String>,
    pub check: Option<String>,
    pub contract: Contract,
    pub paths: Vec<String>,
    pub brief: String,
    pub prompt: Option<String>,
    /// The catalog file `prompt` was read from, when it came from one.
    #[serde(default)]
    pub prompt_file: Option<String>,
    /// Hash of `prompt` as the attempt is given it (includes expanded);
    /// empty when the action has no prompt of its own.
    #[serde(default)]
    pub prompt_hash: String,
    /// The fragments `prompt` includes, with each one's hash.
    #[serde(default)]
    pub includes: Vec<Include>,
    pub schema: Option<String>,
    /// Every outcome an edge may route on: the declared ones, then the
    /// `confidence_below` names.
    #[serde(default)]
    pub outcomes: Vec<String>,
    /// The declared outcomes' descriptions, the criteria of a jev judgment.
    #[serde(default)]
    pub outcome_criteria: BTreeMap<String, String>,
    #[serde(default)]
    pub questions: Vec<Question>,
    /// Confidence floors, ascending: a judgment below the threshold takes
    /// the outcome.
    #[serde(default)]
    pub confidence_below: Vec<(f64, String)>,
    pub file_into_initiative: bool,
    pub overlay: bool,
    pub verifies: bool,
    pub output: Output,
    pub hash: String,
    pub text: String,
}

impl ActionDef {
    /// The schema a job directive's output is held to: the declared
    /// `schema`, with a required `outcome` field constrained to `outcomes`
    /// when the action declares any (docs/EXECUTION.md, "Outcomes, then
    /// edges").
    pub fn effective_schema(&self) -> Option<String> {
        let schema = self.schema.as_deref()?;
        if self.outcomes.is_empty() {
            return Some(schema.to_string());
        }
        let mut v: serde_json::Value = serde_json::from_str(schema).ok()?;
        let obj = v.as_object_mut()?;
        let props = obj
            .entry("properties")
            .or_insert_with(|| serde_json::json!({}));
        props.as_object_mut()?.insert(
            "outcome".into(),
            serde_json::json!({"type": "string", "enum": self.outcomes}),
        );
        let req = obj
            .entry("required")
            .or_insert_with(|| serde_json::json!([]));
        let req = req.as_array_mut()?;
        if !req.iter().any(|r| r == "outcome") {
            req.push("outcome".into());
        }
        Some(v.to_string())
    }

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
    /// A run workflow's directive step: one sentence saying what a script
    /// cannot do here (docs/EXECUTION.md, rule 4).
    judgment: Option<String>,
    /// A run workflow's failure edges (docs/EXECUTION.md, "Outcomes, then
    /// edges"): an outcome, or `failure`, to a step name, node id or `end`.
    #[serde(default)]
    on: BTreeMap<String, String>,
    /// A run workflow's step: how many times a loop may enter it.
    max_attempts: Option<u32>,
    /// A run workflow's operation step: the `[secrets]` names in the
    /// operator's config it is given, as environment (docs/JOBS.md).
    #[serde(default)]
    secrets: Vec<String>,
    /// A run workflow's operation step: the hosts its egress policy is
    /// opened to, and only that step's (`host`, `host:port`, `*.suffix`).
    #[serde(default)]
    egress: Vec<String>,
    /// A run workflow's operation step: what the step may spend, in USD.
    budget_usd: Option<f64>,
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
    #[serde(default)]
    pub judgment: Option<String>,
    #[serde(default)]
    pub on: BTreeMap<String, String>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub egress: Vec<String>,
    #[serde(default)]
    pub budget_usd: Option<f64>,
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
    /// Stable across the run: `<index>-<action>` (see `edges::node_id`).
    #[serde(default)]
    pub node: String,
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

pub(crate) fn parse_action(path: &Path, text: &str, hash: String) -> Result<ActionDef> {
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
            if raw.run.is_some() || raw.check.is_some() || !raw.required_args.is_empty() {
                bail!(
                    "{}: a directive does not have `run`, `check` or `required_args`",
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
            || raw.prompt_file.is_some()
            || raw.schema.is_some()
            || raw.outcomes.is_some()
            || raw.file_into_initiative)
    {
        bail!(
            "{}: contract, paths, brief, prompt, prompt_file, schema, outcomes, and file_into_initiative apply to directives only",
            path.display()
        );
    }
    if let Some(schema) = &raw.schema {
        let v: serde_json::Value = serde_json::from_str(schema)
            .with_context(|| format!("{}: `schema` is not valid JSON", path.display()))?;
        jsonschema::validator_for(&v)
            .with_context(|| format!("{}: `schema` is not a valid JSON Schema", path.display()))?;
    }
    library::check_prompt_file(path, raw.prompt.is_some(), raw.prompt_file.as_deref())?;
    let judged = judgment::parse(
        path,
        raw.kind == Kind::Operation,
        &raw.outcomes,
        &raw.questions,
        &raw.confidence_below,
    )?;
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
        required_args: raw.required_args,
        check: raw.check,
        contract,
        paths: raw.paths,
        brief: raw.brief,
        prompt_hash: raw.prompt.as_deref().map(text_hash).unwrap_or_default(),
        prompt: raw.prompt,
        prompt_file: raw.prompt_file,
        includes: Vec::new(),
        schema: raw.schema,
        outcomes: judged.outcomes,
        outcome_criteria: judged.outcome_criteria,
        questions: raw.questions,
        confidence_below: judged.confidence_below,
        file_into_initiative: raw.file_into_initiative,
        overlay: raw.overlay,
        verifies: raw.verifies,
        output: raw.output,
        hash,
        text: text.to_string(),
    })
}

pub(crate) fn parse_workflow(path: &Path, text: &str, hash: String) -> Result<Workflow> {
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
    let steps = raw
        .steps
        .into_iter()
        .map(|s| step_ref(path, s, raw.kind))
        .collect::<Result<Vec<_>>>()?;
    if raw.kind == WorkflowKind::Build
        && let Some(s) = steps
            .iter()
            .find(|s| !s.on.is_empty() || s.max_attempts.is_some())
    {
        bail!(
            "{}: step {:?} carries `on` or `max_attempts`; edges are a run workflow's (set kind = \"run\"); build workflows keep the list (docs/EXECUTION.md, \"Outcomes, then edges\")",
            path.display(),
            s.action
                .as_deref()
                .or(s.workflow.as_deref())
                .unwrap_or_default()
        );
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

/// One step as written, checked on its own: it names one of `action` or
/// `workflow`, its limits are positive, and what it declares to be given
/// (`check_step_grants`) is well formed.
fn step_ref(path: &Path, s: StepRaw, kind: WorkflowKind) -> Result<StepRef> {
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
    let step = StepRef {
        action,
        workflow: s.workflow,
        model: s.model,
        max_turns: s.max_turns,
        timeout_secs: s.timeout_secs,
        role: s.role,
        effect: s.effect,
        judgment: s.judgment,
        on: s.on,
        max_attempts: s.max_attempts,
        secrets: s.secrets,
        egress: s.egress,
        budget_usd: s.budget_usd,
    };
    check_step_grants(path, &step, kind)?;
    Ok(step)
}

/// A step's `secrets`, `egress` and `budget_usd` are a run workflow's
/// operation step's: refused on a build workflow's step and on a splice,
/// and their values checked here so a malformed one fails at load, not in
/// the middle of a job.
fn check_step_grants(path: &Path, s: &StepRef, kind: WorkflowKind) -> Result<()> {
    if s.secrets.is_empty() && s.egress.is_empty() && s.budget_usd.is_none() {
        return Ok(());
    }
    let step = s
        .action
        .as_deref()
        .or(s.workflow.as_deref())
        .unwrap_or_default();
    if kind == WorkflowKind::Build {
        bail!(
            "{}: step {step:?} carries `secrets`, `egress` or `budget_usd`; those are a run workflow's operation step's (set kind = \"run\")",
            path.display()
        );
    }
    if s.workflow.is_some() {
        bail!(
            "{}: step {step:?} splices in a workflow and carries `secrets`, `egress` or `budget_usd`; declare them on the spliced workflow's own steps",
            path.display()
        );
    }
    for name in &s.secrets {
        crate::secrets::check_name(name)
            .with_context(|| format!("{}: step {step:?}: secrets", path.display()))?;
    }
    for host in &s.egress {
        crate::egress::Rule::parse(host)
            .with_context(|| format!("{}: step {step:?}: egress", path.display()))?;
    }
    if let Some(b) = s.budget_usd
        && !(b.is_finite() && b > 0.0)
    {
        bail!(
            "{}: step {step:?}: budget_usd must be a positive number of dollars",
            path.display()
        );
    }
    Ok(())
}

/// Whether the workflow named `name`, with its own raw text `text`, writes
/// hidden tests: a step with `action = "tests"`, directly or through a
/// `workflow = ...` reference resolved against `known` (workflow name →
/// its own text). Used where the filesystem resolution `resolve` depends
/// on is not available — the task-shape schema backfill (see
/// docs/ECONOMIST.md, "Task shape"), which only has what a task's own row
/// already stored, never the live workflow directory. Best-effort: a
/// nested reference to a workflow missing from `known`, or text that no
/// longer parses, resolves to `false`; `depth` guards against a cycle.
pub(crate) fn text_writes_hidden_tests(
    name: &str,
    text: &str,
    known: &std::collections::BTreeMap<String, String>,
    depth: u8,
) -> bool {
    if depth > 8 {
        return false;
    }
    let Ok(wf) = parse_workflow(Path::new(&format!("{name}.toml")), text, String::new()) else {
        return false;
    };
    wf.steps.iter().any(|s| match (&s.action, &s.workflow) {
        (Some(a), _) => a.as_str() == "tests",
        (None, Some(child)) => known
            .get(child)
            .is_some_and(|t| text_writes_hidden_tests(child, t, known, depth + 1)),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, rel: &str, text: &str) {
        std::fs::write(home.join("workflows").join(rel), text).unwrap();
    }

    #[test]
    fn text_writes_hidden_tests_sees_through_composition_but_not_a_missing_child() {
        let known: std::collections::BTreeMap<String, String> = BUILTIN_WORKFLOWS
            .iter()
            .map(|(file, text)| (file.trim_end_matches(".toml").to_string(), text.to_string()))
            .collect();
        assert!(
            text_writes_hidden_tests("tdd", known["tdd"].as_str(), &known, 0),
            "declares the tests step directly"
        );
        assert!(
            text_writes_hidden_tests("tdd-reviewed", known["tdd-reviewed"].as_str(), &known, 0),
            "nests tdd, which declares it"
        );
        assert!(
            !text_writes_hidden_tests("direct", known["direct"].as_str(), &known, 0),
            "no tests step anywhere in it"
        );
        assert!(
            !text_writes_hidden_tests(
                "tdd-reviewed",
                known["tdd-reviewed"].as_str(),
                &std::collections::BTreeMap::new(),
                0
            ),
            "the nested workflow is unknown, so it resolves to false rather than guessing"
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
  { action = "extract-job",  role = "read", judgment = "a script cannot read free text and decide what it means" },
  { action = "price-job" },
  { action = "draft-quote",  role = "write", judgment = "a script cannot read free text and decide what it means" },
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

    fn step_workflow(kind: &str, step: &str) -> String {
        format!(
            "name = \"nightly\"\nkind = \"{kind}\"\nsteps = [{step}]\n[trigger]\non = \"manual\"\n"
        )
    }

    #[test]
    fn an_operation_step_may_declare_secrets_egress_and_a_budget() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        let step = "{ action = \"code\", effect = \"row\", secrets = [\"cloudflare_token\"], egress = [\"api.cloudflare.com\"], budget_usd = 0.5 }";
        write(dir.path(), "nightly.toml", &step_workflow("run", step));
        let w = get(dir.path(), "nightly").unwrap().unwrap();
        assert_eq!(w.steps[0].secrets, vec!["cloudflare_token".to_string()]);
        assert_eq!(w.steps[0].egress, vec!["api.cloudflare.com".to_string()]);
        assert_eq!(w.steps[0].budget_usd, Some(0.5));
    }

    #[test]
    fn a_step_declaring_no_grants_has_none() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        let step = "{ action = \"code\", effect = \"row\" }";
        write(dir.path(), "nightly.toml", &step_workflow("run", step));
        let w = get(dir.path(), "nightly").unwrap().unwrap();
        assert!(w.steps[0].secrets.is_empty() && w.steps[0].egress.is_empty());
        assert_eq!(w.steps[0].budget_usd, None);
    }

    #[test]
    fn a_malformed_step_grant_is_refused_at_load() {
        for bad in [
            "secrets = [\"Not A Name\"]",
            "egress = [\"https://api.cloudflare.com/\"]",
            "egress = [\"*.com\"]",
            "budget_usd = 0.0",
            "budget_usd = -1.0",
        ] {
            let dir = tempfile::tempdir().unwrap();
            load_all(dir.path()).unwrap();
            let step = format!("{{ action = \"code\", effect = \"row\", {bad} }}");
            write(dir.path(), "nightly.toml", &step_workflow("run", &step));
            assert!(get(dir.path(), "nightly").is_err(), "{bad}");
        }
    }

    #[test]
    fn step_grants_are_a_run_workflows_operation_steps_only() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        let step = "{ action = \"code\", secrets = [\"cloudflare_token\"] }";
        write(dir.path(), "nightly.toml", &step_workflow("build", step));
        assert!(get(dir.path(), "nightly").is_err());
        let splice = "{ workflow = \"other\", egress = [\"api.cloudflare.com\"] }";
        write(dir.path(), "nightly.toml", &step_workflow("run", splice));
        assert!(get(dir.path(), "nightly").is_err());
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
}
