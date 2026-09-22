//! The one place the CLI's machine-readable rows are shaped. `forge log`,
//! `forge requests` and `forge decisions` each have a text form and a
//! `--json` form; both render from the same struct here so the two forms
//! cannot drift apart. This module assembles documents; `render` turns
//! their raw text into the short, customer-safe lines they carry.

use crate::ctx::Forge;
use crate::store::{
    Decision, JournalStat, Message, RoleRouting, StepStat, Task, TaskRef, TaskState, TaskSummary,
    WorkflowStat,
};
use crate::workflows::Problem;
use crate::{config, plugins};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One row of `forge log` / `forge log --json`: a task as the queue lists
/// it. `id`, `state`, `workflow`, `attempts`, `cost_usd`, `created`, `repo`
/// and `task` are the fields the JSON form has always emitted; `text` and
/// `created_at` are the names every row now shares (`text` mirrors `task`,
/// `created_at` is `created` as unix seconds rather than a localtime string).
#[derive(Serialize)]
pub struct TaskRow {
    pub id: i64,
    pub state: String,
    pub workflow: String,
    pub attempts: i64,
    pub cost_usd: f64,
    pub created: String,
    pub repo: String,
    pub task: String,
    pub text: String,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub project: Option<String>,
    pub initiative: Option<i64>,
}

impl From<&TaskSummary> for TaskRow {
    fn from(s: &TaskSummary) -> Self {
        TaskRow {
            id: s.id,
            state: s.state.clone(),
            workflow: s.workflow.clone(),
            attempts: s.attempts,
            cost_usd: s.cost,
            created: s.created.clone(),
            repo: s.repo.clone(),
            task: s.task.clone(),
            text: s.task.clone(),
            created_at: s.created_at,
            finished_at: s.finished_at,
            project: s.project.clone(),
            initiative: s.initiative,
        }
    }
}

/// Sort a blocked task's `reason` into the request `kind` and its display
/// text. The one place that decides what a blocked task is waiting on, so
/// the text and `--json` forms of `forge requests` never disagree.
pub fn request_kind(reason: &str) -> (&'static str, String) {
    if reason.starts_with("waits on task") {
        return ("dependency", reason.to_string());
    }
    match reason.split_once(": ") {
        Some(("needs workflow", rest)) => ("workflow", rest.to_string()),
        Some(("needs suite", rest)) => ("suite", rest.to_string()),
        Some(("needs input", rest)) => ("question", rest.to_string()),
        Some(("review demoted", rest)) => ("review", rest.to_string()),
        _ => ("other", reason.to_string()),
    }
}

/// One row of `forge requests` / `forge requests --json`: a blocked task
/// and what it is waiting on. `id`, `kind`, `text`, `tried`, `path`,
/// `workflow`, `repo` and `task` are the fields the JSON form has always
/// emitted; `question` is the unified name (it mirrors `text`). `tried` and
/// `path` come from the blocking attempt's envelope, not from the task
/// alone, so callers fill them in after building the row from a `Task`.
#[derive(Serialize)]
pub struct RequestRow {
    pub id: i64,
    pub kind: &'static str,
    /// Who the question is addressed to (a channel contact's name);
    /// absent means the operator.
    pub to: Option<String>,
    pub text: String,
    pub question: String,
    pub tried: String,
    pub path: String,
    pub workflow: String,
    pub repo: String,
    pub task: String,
}

impl From<&Task> for RequestRow {
    fn from(t: &Task) -> Self {
        let (kind, text) = request_kind(&t.reason);
        RequestRow {
            id: t.id,
            kind,
            to: t.question_to.clone(),
            text: text.clone(),
            question: text,
            tried: String::new(),
            path: String::new(),
            workflow: t.workflow.clone(),
            repo: t.repo.clone(),
            task: t.task.clone(),
        }
    }
}

/// One row of `forge decisions` / `forge decisions --json`: an operator's
/// answer to a blocked task's question, plus `outcome`, the state of the
/// task the answer re-queued (`None` until the answer is known to have
/// landed, failed, or otherwise settled). `task_id` is `null` for a
/// task-less administrative decision (`forge stats --reprice`).
#[derive(Serialize)]
pub struct DecisionRow {
    pub id: i64,
    pub task_id: Option<i64>,
    pub repo: String,
    pub question: String,
    pub answer: String,
    pub created_at: i64,
    pub answered_by: String,
    pub citations: String,
    pub retry_id: Option<i64>,
    pub outcome: Option<String>,
    /// Who the question was addressed to; absent means the operator.
    pub answered_for: Option<String>,
}

impl DecisionRow {
    pub fn new(d: &Decision, outcome: Option<TaskState>) -> Self {
        DecisionRow {
            id: d.id,
            task_id: d.task_id,
            repo: d.repo.clone(),
            question: d.question.clone(),
            answer: d.answer.clone(),
            created_at: d.created_at,
            answered_by: d.answered_by.clone(),
            citations: d.citations.clone(),
            retry_id: d.retry_id,
            outcome: outcome.map(|s| s.as_str().to_string()),
            answered_for: d.answered_for.clone(),
        }
    }
}

/// One row of `TraceDoc.task.refs` / `forge ref list --json`: an external
/// reference recorded on a task, mirrors `store::TaskRef`.
#[derive(Serialize)]
pub struct RefRow {
    pub id: i64,
    pub task_id: i64,
    pub kind: String,
    pub url: String,
    pub label: String,
    pub by: String,
    pub created_at: i64,
}

impl From<&TaskRef> for RefRow {
    fn from(r: &TaskRef) -> Self {
        RefRow {
            id: r.id,
            task_id: r.task_id,
            kind: r.kind.clone(),
            url: r.url.clone(),
            label: r.label.clone(),
            by: r.by.clone(),
            created_at: r.created_at,
        }
    }
}

/// One row of `forge message list --json`: a message recorded on a
/// channel, mirrors `store::Message`.
#[derive(Serialize)]
pub struct MessageRow {
    pub id: i64,
    pub project: String,
    pub channel: String,
    pub contact: String,
    pub direction: String,
    pub text: String,
    pub at: i64,
    pub task_id: Option<i64>,
}

impl From<&Message> for MessageRow {
    fn from(m: &Message) -> Self {
        MessageRow {
            id: m.id,
            project: m.project.clone(),
            channel: m.channel.clone(),
            contact: m.contact.clone(),
            direction: m.direction.as_str().to_string(),
            text: m.text.clone(),
            at: m.at,
            task_id: m.task_id,
        }
    }
}

/// One task in `TraceDoc.task.lineage`: mirrors `store::LineageRow`, with
/// `cost` renamed `cost_usd` to match the rest of the document.
#[derive(Serialize)]
pub struct TraceLineage {
    pub id: i64,
    pub parent: Option<i64>,
    pub state: String,
    pub reason: String,
    pub workflow: String,
    pub cost_usd: f64,
}

/// The task half of `TraceDoc`: every key `forge trace --json` has always
/// emitted under `"task"`, unchanged. `worktree_removed_at` and
/// `decisions` are not part of that historical shape (`forge show` needs
/// them, `trace --json` never has), so they are skipped on serialize.
#[derive(Serialize)]
pub struct TraceTask {
    pub id: i64,
    pub repo: String,
    pub text: String,
    pub state: String,
    pub reason: String,
    pub workflow: String,
    pub workflow_hash: String,
    pub workflow_text: String,
    pub base_branch: String,
    pub base_sha: String,
    pub branch: String,
    pub worktree: String,
    pub model: String,
    pub provider: String,
    pub max_turns: i64,
    pub max_attempts: i64,
    pub timeout_secs: i64,
    pub checks: Vec<String>,
    pub show_checks: bool,
    pub allow_protected: bool,
    pub land: bool,
    pub after: Vec<i64>,
    pub verify_base: String,
    pub retry_of: Option<i64>,
    pub journal_enabled: bool,
    /// How `journal_enabled` got its value: "explicit", "control", or
    /// "treatment" (see `queue::assign_journal_arm`).
    pub journal_arm: String,
    /// Which provider each role drew from `[measure] explore`, keyed by
    /// role name; empty when the task named an explicit `--provider` or no
    /// role explored (see `queue::assign_explore`).
    pub explore: std::collections::BTreeMap<String, String>,
    /// The routing record (see docs/ECONOMIST.md, "The routing record"):
    /// per role that ran, the provider, model, and workflow it ran under,
    /// each with its source (`"flag"`, `"project"`, `"operator"`,
    /// `"default"`, or `"experiment"`). A role that never ran has no key.
    pub routing: std::collections::BTreeMap<String, RoleRouting>,
    pub context_enabled: bool,
    pub context: String,
    pub resume_on_failure: bool,
    pub parent: Option<i64>,
    pub children: Vec<i64>,
    pub root: i64,
    pub lineage: Vec<TraceLineage>,
    pub refs: Vec<RefRow>,
    pub journal: Option<String>,
    pub interface: String,
    pub plan: String,
    pub pushed: bool,
    pub budget_usd: Option<f64>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub project: Option<String>,
    pub initiative: Option<i64>,
    /// The economist's task-shape inputs (see docs/ECONOMIST.md, "Task
    /// shape"), computed once at enqueue and never revisited.
    pub inputs: TraceTaskShape,
    #[serde(skip)]
    pub worktree_removed_at: Option<i64>,
    #[serde(skip)]
    pub decisions: Vec<Decision>,
}

/// `TraceTask.inputs`: what the economist must condition on before the
/// task even runs (docs/ECONOMIST.md, "Task shape"), grouped here even
/// though `project` also has its own top-level field, so a reader doesn't
/// have to hunt across the rest of the document for the rest of them.
#[derive(Serialize)]
pub struct TraceTaskShape {
    pub text_len: i64,
    pub path_tokens: i64,
    pub tdd: bool,
    pub declared_checks: i64,
    pub project: Option<String>,
}

/// Token counts from an attempt's result frame, as `TraceDoc` nests them.
#[derive(Serialize)]
pub struct TraceTokens {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_creation: Option<i64>,
}

/// Rate-limit usage samples from an attempt, as `TraceDoc` nests them.
#[derive(Serialize)]
pub struct TraceRateLimits {
    pub five_hour: Option<f64>,
    pub seven_day: Option<f64>,
}

/// One attempt in `TraceDoc.attempts`: every key `forge trace --json` has
/// always emitted per attempt, unchanged. `inputs`, `outputs`, `verdict`
/// and `envelope` stay raw JSON, exactly as trace has always rendered them,
/// so callers that parse them into `audit::Inputs` / `audit::Outputs` /
/// `Vec<checks::CheckResult>` / `envelope::Envelope` keep working
/// unchanged. `result_text` is not part of that historical shape (`forge
/// show` needs it, `trace --json` never has), so it is skipped on
/// serialize.
#[derive(Serialize)]
pub struct TraceAttempt {
    pub attempt_no: i64,
    pub step: String,
    pub step_seq: i64,
    pub state: String,
    pub reason: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub agent_exit: Option<i32>,
    pub timed_out: bool,
    pub num_turns: i64,
    pub tool_calls: i64,
    pub cost_usd: Option<f64>,
    pub agent_ms: i64,
    pub commits: i64,
    pub files_changed: i64,
    pub dirty: bool,
    pub start_sha: String,
    pub end_sha: String,
    pub log_path: String,
    pub runner: String,
    pub provider: String,
    pub tokens: TraceTokens,
    pub inputs: Value,
    pub outputs: Value,
    pub verdict: Value,
    pub envelope: Value,
    pub rate_limits: TraceRateLimits,
    #[serde(skip)]
    pub result_text: String,
}

/// One operation in `TraceDoc.ops`: every key `forge trace --json` has
/// always emitted per op, unchanged.
#[derive(Serialize)]
pub struct TraceOp {
    pub id: i64,
    pub seq: i64,
    pub name: String,
    pub kernel: bool,
    pub started_at: i64,
    pub ms: i64,
    pub ok: bool,
    pub exit: Option<i32>,
    pub detail: String,
    pub attempt_id: Option<i64>,
    pub output: String,
}

/// One line of `TraceDoc.diagnosis`: mirrors `audit::Diagnosis`.
#[derive(Serialize)]
pub struct TraceDiagnosis {
    pub what: String,
    pub action: String,
}

/// One finding in `TraceAssessment.findings`, as the assess directive
/// returned it (see src/assess.rs).
#[derive(Serialize, Deserialize, Clone)]
pub struct Finding {
    pub path: String,
    pub finding: String,
    pub severity: String,
}

/// `TraceDoc.assessment`: the assess directive's most recent run against
/// this task's own landing (see docs/ACTIONS.md, "Assessment"). `None`
/// when the workflow never opted in, the task never landed, or the run
/// failed.
#[derive(Serialize)]
pub struct TraceAssessment {
    pub score: i64,
    pub findings: Vec<Finding>,
    pub model: String,
    pub provider: String,
    pub cost_usd: Option<f64>,
    pub created_at: i64,
}

/// Everything `forge trace` shows about a task, built once from the store.
/// `forge trace --json` serializes this directly; `forge trace` and `forge
/// show` both render text from it, so the three no longer each query the
/// store their own way.
#[derive(Serialize)]
pub struct TraceDoc {
    pub task: TraceTask,
    pub attempts: Vec<TraceAttempt>,
    pub ops: Vec<TraceOp>,
    pub resolved: Value,
    pub diagnosis: Vec<TraceDiagnosis>,
    /// This task's deploys, newest first: the on-landing targets it
    /// triggered when it landed (see docs/DEPLOY.md, "When a deploy
    /// runs"). Empty for a task that never landed or landed nothing
    /// on-landing.
    pub deploys: Vec<crate::store::Deploy>,
    /// The assess directive's most recent run against this task's own
    /// landing (see docs/ACTIONS.md, "Assessment"). `None` when it never
    /// ran.
    pub assessment: Option<TraceAssessment>,
}

pub fn trace_doc(f: &Forge, t: &Task) -> Result<TraceDoc> {
    let attempts = f.store.attempts(t.id)?;
    let ops = f.store.ops(t.id)?;
    let diagnosis = crate::audit::diagnose(t, &attempts);
    let lineage = f.store.lineage(t.id)?;
    let deploys = f.store.deploys_for_task(t.id)?;
    let assessment = f.store.assessment(t.id)?.map(|a| TraceAssessment {
        score: a.score,
        findings: serde_json::from_str(&a.findings_json).unwrap_or_default(),
        model: a.model,
        provider: a.provider,
        cost_usd: a.cost_usd,
        created_at: a.created_at,
    });

    let task = TraceTask {
        id: t.id,
        repo: t.repo.clone(),
        text: t.task.clone(),
        state: t.state.as_str().to_string(),
        reason: t.reason.clone(),
        workflow: t.workflow.clone(),
        workflow_hash: t.workflow_hash.clone(),
        workflow_text: t.workflow_text.clone(),
        base_branch: t.base_branch.clone(),
        base_sha: t.base_sha.clone(),
        branch: t.branch.clone(),
        worktree: t.worktree.clone(),
        model: t.model.clone(),
        provider: t.provider.clone(),
        max_turns: t.max_turns,
        max_attempts: t.max_attempts,
        timeout_secs: t.timeout_secs,
        checks: t.checks.clone(),
        show_checks: t.show_checks,
        allow_protected: t.allow_protected,
        land: t.land,
        after: t.after.clone(),
        verify_base: t.verify_base.clone(),
        retry_of: t.retry_of,
        journal_enabled: t.journal,
        journal_arm: t.journal_arm.clone(),
        explore: t.explore.clone(),
        routing: t.routing.clone(),
        context_enabled: t.context_enabled,
        context: t.context.clone(),
        resume_on_failure: t.resume_on_failure,
        parent: t.retry_of,
        children: f.store.dependents_retries(t.id)?,
        root: f.store.root_of(t.id)?,
        lineage: lineage
            .iter()
            .map(|l| TraceLineage {
                id: l.id,
                parent: l.parent,
                state: l.state.clone(),
                reason: l.reason.clone(),
                workflow: l.workflow.clone(),
                cost_usd: l.cost,
            })
            .collect(),
        refs: f.store.task_refs(t.id)?.iter().map(RefRow::from).collect(),
        journal: crate::journal::journal_for(f, t)
            .ok()
            .filter(|j| !j.is_empty()),
        interface: t.interface.clone(),
        plan: t.plan.clone(),
        pushed: t.pushed,
        budget_usd: t.budget_usd,
        created_at: t.created_at,
        started_at: t.started_at,
        finished_at: t.finished_at,
        project: t.project.clone(),
        initiative: t.initiative,
        inputs: TraceTaskShape {
            text_len: t.shape_text_len,
            path_tokens: t.shape_path_tokens,
            tdd: t.shape_tdd,
            declared_checks: t.shape_declared_checks,
            project: t.project.clone(),
        },
        worktree_removed_at: t.worktree_removed_at,
        decisions: f.store.decisions_in_lineage(t.id)?,
    };

    let attempts = attempts
        .iter()
        .map(|a| TraceAttempt {
            attempt_no: a.attempt_no,
            step: a.step.clone(),
            step_seq: a.step_seq,
            state: a.state.as_str().to_string(),
            reason: a.reason.clone(),
            started_at: a.started_at,
            finished_at: a.finished_at,
            agent_exit: a.agent_exit,
            timed_out: a.timed_out,
            num_turns: a.num_turns,
            tool_calls: a.tool_calls,
            cost_usd: a.cost_usd,
            agent_ms: a.agent_ms,
            commits: a.commits,
            files_changed: a.files_changed,
            dirty: a.dirty,
            start_sha: a.start_sha.clone(),
            end_sha: a.end_sha.clone(),
            log_path: a.log_path.clone(),
            runner: a.runner.clone(),
            provider: a.provider.clone(),
            tokens: TraceTokens {
                input: a.input_tokens,
                output: a.output_tokens,
                cache_read: a.cache_read_input_tokens,
                cache_creation: a.cache_creation_input_tokens,
            },
            inputs: serde_json::from_str(&a.inputs_json).unwrap_or_default(),
            outputs: serde_json::from_str(&a.outputs_json).unwrap_or_default(),
            verdict: serde_json::from_str(&a.verdict_json).unwrap_or_default(),
            envelope: serde_json::from_str(&a.envelope_json).unwrap_or_default(),
            rate_limits: TraceRateLimits {
                five_hour: a.rl_five_hour,
                seven_day: a.rl_seven_day,
            },
            result_text: a.result_text.clone(),
        })
        .collect();

    let ops = ops
        .iter()
        .map(|o| TraceOp {
            id: o.id,
            seq: o.seq,
            name: o.name.clone(),
            kernel: o.kernel,
            started_at: o.started_at,
            ms: o.ms,
            ok: o.ok,
            exit: o.exit,
            detail: o.detail.clone(),
            attempt_id: o.attempt_id,
            output: o.output.clone(),
        })
        .collect();

    let resolved = serde_json::from_str(&t.actions_json).unwrap_or_default();

    let diagnosis = diagnosis
        .into_iter()
        .map(|d| TraceDiagnosis {
            what: d.what,
            action: d.action,
        })
        .collect();

    Ok(TraceDoc {
        task,
        attempts,
        ops,
        resolved,
        diagnosis,
        deploys,
        assessment,
    })
}

/// One row of `StatsDoc.workflows`: outcomes for one workflow (name +
/// definition hash), as both `forge stats` and `forge stats --json` show
/// it. `legacy` carries the header-named keys (`WF`, `HASH`, `TASKS`,
/// `OK`, `FAIL`, `BLK`, `UNV`, `ATT`, `COST`, `$/OK`, `LANDED`,
/// `$/LANDED`) the JSON form emitted before the named fields below
/// existed; it is flattened onto this row so both sets of keys are
/// present on `--json` output. Deprecated: kept for one release only,
/// read the named fields instead.
#[derive(Serialize)]
pub struct StatsWorkflowRow {
    /// Workflow name.
    pub workflow: String,
    /// Hash of the workflow definition this row's tasks ran with.
    pub hash: String,
    /// Number of tasks run under this workflow + hash.
    pub pieces: i64,
    /// Tasks that finished in state `succeeded`.
    pub succeeded: i64,
    /// Tasks that finished in state `failed`.
    pub failed: i64,
    /// Tasks that finished in state `blocked`.
    pub blocked: i64,
    /// Tasks that finished in state `unverified`.
    pub unverified: i64,
    /// Attempts run across all of this workflow's tasks.
    pub attempts: i64,
    /// Cost, in USD, of every attempt across this workflow's tasks.
    pub mean_cost_usd: f64,
    /// `mean_cost_usd` divided by `succeeded`; `None` when nothing succeeded.
    pub cost_per_success_usd: Option<f64>,
    /// Tasks that landed on their base branch.
    pub landed: i64,
    /// `mean_cost_usd` divided by `landed`; `None` when nothing landed.
    pub cost_per_landed_usd: Option<f64>,
    /// Landed tasks whose `landed_sha` became a later task's `base_sha`,
    /// where that later task's first `code` attempt carries a failing L1
    /// verdict row on an unmodified base.
    pub broke_base: i64,
    /// `broke_base` divided by `landed`; `None` when nothing landed.
    pub broke_base_share: Option<f64>,
    /// Landed tasks named by a later task's `repairs` reference.
    pub repaired: i64,
    /// `repaired` divided by `landed`; `None` when nothing landed.
    pub repaired_share: Option<f64>,
    /// Delayed cost, line-overlap attribution: for each later landing on
    /// the same repository within 30 days, the fraction of its cost equal
    /// to the lines it removed or rewrote that a landed task's own
    /// landing added, divided by all the lines it removed or rewrote —
    /// summed across this workflow's landed tasks (see docs/LATER.md, the
    /// delayed-cost follow-up to "Defect escape"). `Store::WorkflowStat::repair_cost`.
    pub repair_cost_usd: f64,
    /// `(mean_cost_usd + repair_cost_usd) / landed`: what a piece of
    /// work in this workflow actually cost once its delayed cost is in,
    /// not only what landing it cost. `None` when nothing landed.
    pub true_cost_per_landed_usd: Option<f64>,
    /// Churn: of the lines this workflow's landed tasks added, the share
    /// a later landing on the same repository removed or rewrote within
    /// 30 days (`task_churn`, cached per task). `None` when nothing was
    /// added yet to measure.
    pub churn_share: Option<f64>,
    #[serde(flatten)]
    pub legacy: serde_json::Map<String, Value>,
}

impl From<&WorkflowStat> for StatsWorkflowRow {
    fn from(w: &WorkflowStat) -> Self {
        let cost_per_success_usd = (w.succeeded > 0).then(|| w.cost / w.succeeded as f64);
        let cost_per_landed_usd = (w.landed > 0).then(|| w.cost / w.landed as f64);
        let broke_base_share = (w.landed > 0).then(|| w.broke_base as f64 / w.landed as f64);
        let repaired_share = (w.landed > 0).then(|| w.repaired as f64 / w.landed as f64);
        let true_cost_per_landed_usd =
            (w.landed > 0).then(|| (w.cost + w.repair_cost) / w.landed as f64);
        let churn_share =
            (w.added_lines > 0).then(|| w.churned_lines as f64 / w.added_lines as f64);
        let mut legacy = serde_json::Map::new();
        legacy.insert("WF".into(), Value::from(w.workflow.clone()));
        legacy.insert("HASH".into(), Value::from(w.hash.clone()));
        legacy.insert("TASKS".into(), Value::from(w.tasks));
        legacy.insert("OK".into(), Value::from(w.succeeded));
        legacy.insert("FAIL".into(), Value::from(w.failed));
        legacy.insert("BLK".into(), Value::from(w.blocked));
        legacy.insert("UNV".into(), Value::from(w.unverified));
        legacy.insert("ATT".into(), Value::from(w.attempts));
        legacy.insert("COST".into(), Value::from(w.cost));
        legacy.insert("$/OK".into(), serde_json::json!(cost_per_success_usd));
        legacy.insert("LANDED".into(), Value::from(w.landed));
        legacy.insert("$/LANDED".into(), serde_json::json!(cost_per_landed_usd));
        StatsWorkflowRow {
            workflow: w.workflow.clone(),
            hash: w.hash.clone(),
            pieces: w.tasks,
            succeeded: w.succeeded,
            failed: w.failed,
            blocked: w.blocked,
            unverified: w.unverified,
            attempts: w.attempts,
            mean_cost_usd: w.cost,
            cost_per_success_usd,
            landed: w.landed,
            cost_per_landed_usd,
            broke_base: w.broke_base,
            broke_base_share,
            repaired: w.repaired,
            repaired_share,
            repair_cost_usd: w.repair_cost,
            true_cost_per_landed_usd,
            churn_share,
            legacy,
        }
    }
}

/// One row of `StatsDoc.steps`: outcomes for one workflow step, as both
/// `forge stats` and `forge stats --json` show it. `legacy` carries the
/// header-named keys (`WF`, `STEP`, `ATT`, `OK`, `AGENTF`, `CHECKF`,
/// `ASK`, `TURNS`, `EDIT@`, `SECS`, `COST`, `TOKENS`) the JSON form
/// emitted before the named fields below existed; it is flattened onto
/// this row so both sets of keys are present on `--json` output.
/// Deprecated: kept for one release only, read the named fields instead.
#[derive(Serialize)]
pub struct StatsStepRow {
    /// Workflow name.
    pub workflow: String,
    /// Step name within the workflow.
    pub step: String,
    /// Attempts run at this step.
    pub attempts: i64,
    /// Attempts that finished in state `succeeded`.
    pub succeeded: i64,
    /// Attempts that finished in state `agent_failed`.
    pub agent_failed: i64,
    /// Attempts that finished in state `checks_failed`.
    pub checks_failed: i64,
    /// Attempts that finished in state `needs_input`.
    pub needs_input: i64,
    /// Mean number of agent turns per attempt.
    pub mean_turns: f64,
    /// Mean tool calls before the first edit, over attempts that edited;
    /// `None` when none did.
    pub mean_first_edit: Option<f64>,
    /// Mean wall-clock seconds per attempt.
    pub mean_secs: f64,
    /// Cost, in USD, of every attempt at this step.
    pub cost_usd: f64,
    /// Mean input tokens, over attempts that reported usage; `None` when
    /// none did.
    pub mean_input_tokens: Option<f64>,
    #[serde(flatten)]
    pub legacy: serde_json::Map<String, Value>,
}

impl From<&StepStat> for StatsStepRow {
    fn from(st: &StepStat) -> Self {
        let mean_secs = st.mean_ms / 1000.0;
        let mut legacy = serde_json::Map::new();
        legacy.insert("WF".into(), Value::from(st.workflow.clone()));
        legacy.insert("STEP".into(), Value::from(st.step.clone()));
        legacy.insert("ATT".into(), Value::from(st.attempts));
        legacy.insert("OK".into(), Value::from(st.succeeded));
        legacy.insert("AGENTF".into(), Value::from(st.agent_failed));
        legacy.insert("CHECKF".into(), Value::from(st.checks_failed));
        legacy.insert("ASK".into(), Value::from(st.needs_input));
        legacy.insert("TURNS".into(), Value::from(st.mean_turns));
        legacy.insert("EDIT@".into(), serde_json::json!(st.mean_first_edit));
        legacy.insert("SECS".into(), Value::from(mean_secs));
        legacy.insert("COST".into(), Value::from(st.cost));
        legacy.insert("TOKENS".into(), serde_json::json!(st.mean_input_tokens));
        StatsStepRow {
            workflow: st.workflow.clone(),
            step: st.step.clone(),
            attempts: st.attempts,
            succeeded: st.succeeded,
            agent_failed: st.agent_failed,
            checks_failed: st.checks_failed,
            needs_input: st.needs_input,
            mean_turns: st.mean_turns,
            mean_first_edit: st.mean_first_edit,
            mean_secs,
            cost_usd: st.cost,
            mean_input_tokens: st.mean_input_tokens,
            legacy,
        }
    }
}

/// One side of `StatsDoc.journal` / `StatsDoc.no_journal`: code attempts
/// after the first (`attempt_no > 1`), for attempts that either were or
/// were not handed a journal. See docs/LATER.md, "The journal measurement
/// was ill-posed three times".
#[derive(Serialize, Default)]
pub struct StatsJournalRow {
    /// Code retries in this arm.
    pub attempts: i64,
    /// Of those, how many finished in state `succeeded`.
    pub succeeded: i64,
    /// `succeeded` divided by `attempts`; `None` when there are none.
    pub succeeded_share: Option<f64>,
    /// Mean agent turns per attempt.
    pub mean_turns: f64,
    /// Mean tool calls before the first edit, over attempts that edited;
    /// `None` when none did.
    pub mean_first_edit: Option<f64>,
    /// Mean cost in USD per attempt.
    pub mean_cost_usd: f64,
}

impl From<&JournalStat> for StatsJournalRow {
    fn from(j: &JournalStat) -> Self {
        StatsJournalRow {
            attempts: j.attempts,
            succeeded: j.succeeded,
            succeeded_share: (j.attempts > 0).then(|| j.succeeded as f64 / j.attempts as f64),
            mean_turns: j.mean_turns,
            mean_first_edit: j.mean_first_edit,
            mean_cost_usd: j.mean_cost_usd,
        }
    }
}

/// One row of `StatsDoc.projects`: tasks, landed count, cost and defect
/// escape for one project, shown when `forge stats` is not itself scoped
/// to a project or initiative.
#[derive(Serialize)]
pub struct StatsProjectRow {
    pub project: String,
    pub tasks: i64,
    pub landed: i64,
    pub cost_usd: f64,
    /// See `StatsWorkflowRow::broke_base`.
    pub broke_base: i64,
    /// `broke_base` divided by `landed`; `None` when nothing landed.
    pub broke_base_share: Option<f64>,
}

impl From<&crate::store::ProjectStat> for StatsProjectRow {
    fn from(p: &crate::store::ProjectStat) -> Self {
        StatsProjectRow {
            project: p.project.clone(),
            tasks: p.tasks,
            landed: p.landed,
            cost_usd: p.cost,
            broke_base: p.broke_base,
            broke_base_share: (p.landed > 0).then(|| p.broke_base as f64 / p.landed as f64),
        }
    }
}

/// One row of `StatsDoc.human_attention`: human attention for one
/// workflow version — what a person had to do for its landed work, since
/// minutes cannot be measured (docs/LATER.md, "Two metrics the record can
/// compute and does not"). Four events, summed as `events` and divided by
/// `landed` as `events_per_landed`.
#[derive(Serialize)]
pub struct HumanAttentionRow {
    pub workflow: String,
    pub hash: String,
    pub landed: i64,
    /// Decisions on this workflow's tasks with `answered_by` other than
    /// `"supervisor"` (an operator, or a channel contact).
    pub operator_answers: i64,
    /// This workflow's tasks landed by a human's `forge land`.
    pub hand_landed: i64,
    /// This workflow's tasks left `withdrawn`.
    pub withdrawals: i64,
    /// Commits not authored as Forge, on the base branch, between this
    /// workflow's landings and the ones before them.
    pub hand_commits: i64,
    /// The four counts above, summed.
    pub events: i64,
    /// `events` divided by `landed`; `None` when nothing landed.
    pub events_per_landed: Option<f64>,
}

impl From<&crate::store::HumanAttentionStat> for HumanAttentionRow {
    fn from(h: &crate::store::HumanAttentionStat) -> Self {
        let events = h.operator_answers + h.hand_landed + h.withdrawals + h.hand_commits;
        HumanAttentionRow {
            workflow: h.workflow.clone(),
            hash: h.hash.clone(),
            landed: h.landed,
            operator_answers: h.operator_answers,
            hand_landed: h.hand_landed,
            withdrawals: h.withdrawals,
            hand_commits: h.hand_commits,
            events,
            events_per_landed: (h.landed > 0).then(|| events as f64 / h.landed as f64),
        }
    }
}

/// One row of `StatsDoc.human_attention_projects`: the same four signals
/// as `HumanAttentionRow`, over one project's tasks instead of one
/// workflow version's; shown under the same scoping rule as `projects`.
#[derive(Serialize)]
pub struct HumanAttentionProjectRow {
    pub project: String,
    pub landed: i64,
    pub operator_answers: i64,
    pub hand_landed: i64,
    pub withdrawals: i64,
    pub hand_commits: i64,
    pub events: i64,
    pub events_per_landed: Option<f64>,
}

impl From<&crate::store::HumanAttentionProjectStat> for HumanAttentionProjectRow {
    fn from(h: &crate::store::HumanAttentionProjectStat) -> Self {
        let events = h.operator_answers + h.hand_landed + h.withdrawals + h.hand_commits;
        HumanAttentionProjectRow {
            project: h.project.clone(),
            landed: h.landed,
            operator_answers: h.operator_answers,
            hand_landed: h.hand_landed,
            withdrawals: h.withdrawals,
            hand_commits: h.hand_commits,
            events,
            events_per_landed: (h.landed > 0).then(|| events as f64 / h.landed as f64),
        }
    }
}

/// One row of `StatsDoc.time_to_live`: how long a request took to go live,
/// for one workflow version's landed tasks (docs/LATER.md, "Two metrics
/// the record can compute and does not"). Per task, that is
/// `landed_at - created_at`, or, when a deploy is tied to the task, that
/// deploy's `finished_at - created_at` instead (see `Store::task_ttls`).
#[derive(Serialize)]
pub struct TimeToLiveRow {
    pub workflow: String,
    pub hash: String,
    /// How many landed tasks this rests on.
    pub n: i64,
    /// `None` when `n` is 0.
    pub median_secs: Option<f64>,
    /// `None` when `n` is 0.
    pub p90_secs: Option<f64>,
}

/// One row of `StatsDoc.time_to_live_projects`: the same measure as
/// `TimeToLiveRow`, over one project's landed tasks instead of one
/// workflow version's; shown under the same scoping rule as `projects`.
#[derive(Serialize)]
pub struct TimeToLiveProjectRow {
    pub project: String,
    pub n: i64,
    pub median_secs: Option<f64>,
    pub p90_secs: Option<f64>,
}

/// The median and 90th percentile of `secs`, nearest-rank on the sorted
/// list (so the percentile is always one of the actual values, never an
/// interpolation) — `(None, None)` when `secs` is empty.
fn median_p90(secs: &mut [i64]) -> (Option<f64>, Option<f64>) {
    if secs.is_empty() {
        return (None, None);
    }
    secs.sort_unstable();
    let at = |p: f64| -> f64 {
        let rank = ((p * secs.len() as f64).ceil() as usize)
            .max(1)
            .min(secs.len());
        secs[rank - 1] as f64
    };
    (Some(at(0.5)), Some(at(0.9)))
}

/// One row of `StatsDoc.jobs`: one project's jobs in the last rolling 24h,
/// by outcome — counted separately from `StatsProjectRow`'s task rollup
/// (docs/JOBS.md step 1d).
#[derive(Serialize)]
pub struct StatsJobsRow {
    pub project: String,
    pub today: i64,
    pub ok: i64,
    pub failed: i64,
    pub needs_human: i64,
    pub skipped: i64,
}

impl From<&crate::store::JobStat> for StatsJobsRow {
    fn from(j: &crate::store::JobStat) -> Self {
        StatsJobsRow {
            project: j.project.clone(),
            today: j.today,
            ok: j.ok,
            failed: j.failed,
            needs_human: j.needs_human,
            skipped: j.skipped,
        }
    }
}

/// One row of `StatsDoc.by_role`: attempts, outcomes, cost and wall time
/// for one (role, provider, model, kind) combination, role being the
/// attempt's step or the job step's action, and kind (`"attempt"` or
/// `"job_step"`) distinguishing a build task's attempt from a job's
/// directive step, as `forge stats --by-role` shows it.
#[derive(Serialize)]
pub struct StatsRoleRow {
    pub role: String,
    pub provider: String,
    pub model: String,
    pub kind: String,
    pub attempts: i64,
    pub succeeded: i64,
    /// `succeeded` divided by `attempts`; `None` when there are none.
    pub succeeded_share: Option<f64>,
    pub mean_turns: f64,
    pub mean_cost_usd: f64,
    pub mean_secs: f64,
    /// Landed tasks with an attempt in this group; `None` outside the
    /// `code` role. See `StatsWorkflowRow::broke_base`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landed: Option<i64>,
    /// Of `landed`, how many broke a later task's base; `None` outside the
    /// `code` role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broke_base: Option<i64>,
    /// `broke_base` divided by `landed`; `None` when `landed` is `None` or 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broke_base_share: Option<f64>,
    /// See `StatsWorkflowRow::repair_cost_usd`, summed over this
    /// group's own landed tasks; `None` outside the `code` role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_cost_usd: Option<f64>,
    /// See `StatsWorkflowRow::true_cost_per_landed_usd`; `None` outside
    /// the `code` role or when `landed` is `None` or 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub true_cost_per_landed_usd: Option<f64>,
    /// See `StatsWorkflowRow::churn_share`; `None` outside the `code`
    /// role or when nothing was added yet to measure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub churn_share: Option<f64>,
}

impl From<&crate::store::RoleStat> for StatsRoleRow {
    fn from(r: &crate::store::RoleStat) -> Self {
        let broke_base_share = match (r.landed, r.broke_base) {
            (Some(landed), Some(broke)) if landed > 0 => Some(broke as f64 / landed as f64),
            _ => None,
        };
        let true_cost_per_landed_usd = match (r.landed, r.repair_cost) {
            (Some(landed), Some(repair_cost)) if landed > 0 => {
                Some((r.mean_cost_usd * r.attempts as f64 + repair_cost) / landed as f64)
            }
            _ => None,
        };
        let churn_share = match (r.added_lines, r.churned_lines) {
            (Some(added), Some(churned)) if added > 0 => Some(churned as f64 / added as f64),
            _ => None,
        };
        StatsRoleRow {
            role: r.role.clone(),
            provider: r.provider.clone(),
            model: r.model.clone(),
            kind: r.kind.clone(),
            attempts: r.attempts,
            succeeded: r.succeeded,
            succeeded_share: (r.attempts > 0).then(|| r.succeeded as f64 / r.attempts as f64),
            mean_turns: r.mean_turns,
            mean_cost_usd: r.mean_cost_usd,
            mean_secs: r.mean_ms / 1000.0,
            landed: r.landed,
            broke_base: r.broke_base,
            broke_base_share,
            repair_cost_usd: r.repair_cost,
            true_cost_per_landed_usd,
            churn_share,
        }
    }
}

/// Everything `forge stats` shows: outcomes per workflow, outcomes per
/// step, the journal control arm's retrospective split (with `--journal`),
/// and (with `--tools`) tool usage per step. `forge stats --json`
/// serializes this directly; `forge stats` renders the same tables as
/// text from the same rows.
#[derive(Serialize)]
pub struct StatsDoc {
    pub workflows: Vec<StatsWorkflowRow>,
    pub steps: Vec<StatsStepRow>,
    /// Code retries that were handed a journal.
    pub journal: StatsJournalRow,
    /// Code retries that were not.
    pub no_journal: StatsJournalRow,
    /// Tasks, landed count, cost and defect escape per project; only
    /// filled when `stats_doc` was called with no project/initiative
    /// scope of its own (see docs/PROJECTS.md, "The record, scoped").
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<StatsProjectRow>,
    /// Jobs started in the last rolling 24h, per project, by outcome; same
    /// scoping rule as `projects` (docs/JOBS.md step 1d).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<StatsJobsRow>,
    /// Attempts, outcomes, cost and wall time per (role, provider, model);
    /// see `forge stats --by-role`.
    pub by_role: Vec<StatsRoleRow>,
    /// Spearman's rank correlation between the assess directive's score
    /// and each delayed-cost measure, over this scope's landed tasks
    /// that carry both; see `forge stats --quality` and
    /// `quality_correlation`.
    pub assessment_correlation: Vec<CorrelationRow>,
    /// Human attention per workflow version: what a person had to do for
    /// its landed work (see `HumanAttentionRow`, `forge stats --quality`).
    pub human_attention: Vec<HumanAttentionRow>,
    /// Human attention per project; same scoping rule as `projects`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub human_attention_projects: Vec<HumanAttentionProjectRow>,
    /// Time to live per workflow version: how long a request took to go
    /// live (see `TimeToLiveRow`, `forge stats --quality`).
    pub time_to_live: Vec<TimeToLiveRow>,
    /// Time to live per project; same scoping rule as `projects`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub time_to_live_projects: Vec<TimeToLiveProjectRow>,
    /// One row per factor level, over this scope's landed and failed
    /// tasks in the window `--days` names (every one of them, absent a
    /// window); see `forge stats --factors` and `StatsFactorRow`.
    pub factors: Vec<StatsFactorRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
}

/// One row of `StatsDoc.factors`: one level of one factor in `forge
/// stats --factors` (docs/ECONOMIST.md, piece 3) — `factor` is
/// `"provider:<role>"` (the provider a role ran under, one factor per
/// role that ran in scope), `"workflow"`, or `"size"` (the task-shape
/// bin from `crate::store::size_class`); `level` is that factor's value
/// (a provider name, a workflow name, or `"small"`/`"medium"`/`"large"`).
///
/// `tasks`, `landed`, `rate` (with its Wilson 95% interval, `rate_lo`/
/// `rate_hi`) and `mean_true_cost_usd` (null when nothing in this level
/// landed) describe this level's own tasks. `effect` and `effect_se`
/// come from one joint least-squares fit of `ln(true cost)` on every
/// factor and level in scope at once (main effects only, no
/// interactions): the change in log cost this level carries against its
/// factor's reference level (`is_reference`), with a standard error.
/// Both are null for the reference level itself, and for any level
/// whose factor the fit could not identify (stuck at one level among
/// the tasks that landed, too little data for the columns in play, or
/// perfectly confounded with another factor) — see
/// `crate::store::Store::factor_stats`.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct StatsFactorRow {
    pub factor: String,
    pub level: String,
    pub tasks: i64,
    pub landed: i64,
    pub rate: f64,
    pub rate_lo: f64,
    pub rate_hi: f64,
    pub mean_true_cost_usd: Option<f64>,
    pub is_reference: bool,
    pub effect: Option<f64>,
    pub effect_se: Option<f64>,
}

impl From<&crate::store::FactorLevelStat> for StatsFactorRow {
    fn from(f: &crate::store::FactorLevelStat) -> Self {
        StatsFactorRow {
            factor: f.factor.clone(),
            level: f.level.clone(),
            tasks: f.tasks,
            landed: f.landed,
            rate: f.rate,
            rate_lo: f.rate_lo,
            rate_hi: f.rate_hi,
            mean_true_cost_usd: f.mean_true_cost_usd,
            is_reference: f.is_reference,
            effect: f.effect,
            effect_se: f.effect_se,
        }
    }
}

/// One row of `StatsDoc.assessment_correlation`: how well the assess
/// directive's fast proxy score tracks one delayed-cost measure that only
/// shows up once later tasks land (docs/ACTIONS.md, "Assessment";
/// docs/LATER.md's delayed-cost follow-up to "Defect escape") — Spearman's
/// rank correlation, over this scope's landed tasks carrying both an
/// assessment and the measure.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct CorrelationRow {
    /// `"churn"` (per task, the share of its added lines a later landing
    /// rewrote) or `"repair_cost"` (per task, its cached repair cost in
    /// USD).
    pub measure: String,
    /// Spearman's rho; `None` when fewer than two tasks carry both the
    /// score and this measure, or either side has no variance to rank.
    pub rho: Option<f64>,
    /// How many landed, assessed tasks this rests on.
    pub n: i64,
}

/// Spearman's rank correlation between the assess directive's score and
/// each delayed-cost measure, over `scope`'s landed tasks that carry an
/// assessment and the measure in question: `churn` (per task, its cached
/// `(churned, added)` lines as `churned / added`, omitted when `added` is
/// 0) and `repair_cost` (per task, its cached repair cost in USD; see
/// `refresh_churn` and `refresh_repair_cost`, both already run by the
/// time `stats_doc` calls this). The two measures can rest on different
/// task counts, since a task can have one cached without the other.
fn quality_correlation(
    f: &Forge,
    scope: &crate::store::StatsFilter,
) -> Result<Vec<CorrelationRow>> {
    let mut churn_pairs = Vec::new();
    let mut repair_pairs = Vec::new();
    for t in f.store.landed_tasks(scope)? {
        let Some(a) = f.store.assessment(t.id)? else {
            continue;
        };
        let score = a.score as f64;
        if let Some((added, churned, _)) = f.store.churn_cache(t.id)?
            && added > 0
        {
            churn_pairs.push((score, churned as f64 / added as f64));
        }
        if let Some((repair_cost, _)) = f.store.repair_cost_cache(t.id)? {
            repair_pairs.push((score, repair_cost));
        }
    }
    Ok(vec![
        CorrelationRow {
            measure: "churn".into(),
            n: churn_pairs.len() as i64,
            rho: spearman(&churn_pairs),
        },
        CorrelationRow {
            measure: "repair_cost".into(),
            n: repair_pairs.len() as i64,
            rho: spearman(&repair_pairs),
        },
    ])
}

/// Spearman's rank correlation over `pairs`: Pearson's r computed on each
/// side's ranks (ties broken by averaging), which is Spearman's rho with
/// the standard tie correction. `None` when fewer than two pairs, or
/// either side is constant (an undefined correlation).
fn spearman(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < 2 {
        return None;
    }
    let xs: Vec<f64> = pairs.iter().map(|p| p.0).collect();
    let ys: Vec<f64> = pairs.iter().map(|p| p.1).collect();
    pearson(&rank(&xs), &rank(&ys))
}

/// 1-based ranks of `v`, tied values given their shared average rank.
fn rank(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].partial_cmp(&v[b]).unwrap());
    let mut ranks = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] {
            j += 1;
        }
        let avg_rank = (i + j) as f64 / 2.0 + 1.0;
        for &k in &idx[i..=j] {
            ranks[k] = avg_rank;
        }
        i = j + 1;
    }
    ranks
}

/// Pearson's r between `a` and `b`, `None` when either has zero variance.
fn pearson(a: &[f64], b: &[f64]) -> Option<f64> {
    let n = a.len() as f64;
    let mean_a = a.iter().sum::<f64>() / n;
    let mean_b = b.iter().sum::<f64>() / n;
    let (mut cov, mut var_a, mut var_b) = (0.0, 0.0, 0.0);
    for i in 0..a.len() {
        let da = a[i] - mean_a;
        let db = b[i] - mean_b;
        cov += da * db;
        var_a += da * da;
        var_b += db * db;
    }
    if var_a == 0.0 || var_b == 0.0 {
        return None;
    }
    Some(cov / (var_a.sqrt() * var_b.sqrt()))
}

/// Refresh `task_churn` for every landed task (scope does not narrow this:
/// `by_role` and an unscoped `forge stats` both read the whole table, so a
/// scoped call would leave the rest stale) whose 30-day window has not yet
/// closed as of its last computation, or that has never been computed:
/// the only part of `forge stats` that reads git rather than the store,
/// since churn is a diff over the repository's own history (docs/LATER.md,
/// the delayed-cost follow-up to "Defect escape"). Once a task's window
/// has closed, its cache is never touched again.
async fn refresh_churn(f: &Forge) -> Result<()> {
    let now = crate::unix_now();
    for t in f
        .store
        .landed_tasks(&crate::store::StatsFilter::default())?
    {
        let Some(finished_at) = t.finished_at else {
            continue;
        };
        let window_closes = finished_at + crate::store::THIRTY_DAYS_SECS;
        let stale = match f.store.churn_cache(t.id)? {
            Some((_, _, computed_at)) => computed_at < window_closes,
            None => true,
        };
        if !stale {
            continue;
        }
        let (added, churned) = compute_churn(f, &t, finished_at).await?;
        f.store.set_churn_cache(t.id, added, churned, now)?;
    }
    Ok(())
}

/// One landed task's churn: lines its landing added (`base_sha` to
/// `landed_sha`), and of those, how many exact `(path, content)` pairs a
/// later landing on the same repository removed or rewrote within 30 days
/// of this one landing.
async fn compute_churn(f: &Forge, t: &Task, finished_at: i64) -> Result<(i64, i64)> {
    let repo = std::path::Path::new(&t.repo);
    let (added, _) = crate::git::diff_lines(repo, &t.base_sha, &t.landed_sha).await?;
    if added.is_empty() {
        return Ok((0, 0));
    }
    let window_end = finished_at + crate::store::THIRTY_DAYS_SECS;
    let later = f
        .store
        .later_landings(&t.repo, t.id, finished_at, window_end)?;
    let mut removed_set: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for u in later {
        let (_, removed) = crate::git::diff_lines(repo, &u.base_sha, &u.landed_sha).await?;
        removed_set.extend(removed);
    }
    let churned = added
        .iter()
        .filter(|line| removed_set.contains(*line))
        .count() as i64;
    Ok((added.len() as i64, churned))
}

/// Refresh `task_repair_cost` for every landed task, on the same
/// stale-until-the-window-closes schedule as `refresh_churn`: the REPAIRCOST
/// column, replacing task 357's path-overlap follow-on cost (which charged
/// a landed task the whole cost of any later task that changed any path it
/// changed) with a line-level attribution — a later landing only charges an
/// earlier one for the fraction of its own cost spent rewriting that
/// earlier task's actual lines.
async fn refresh_repair_cost(f: &Forge) -> Result<()> {
    let now = crate::unix_now();
    for t in f
        .store
        .landed_tasks(&crate::store::StatsFilter::default())?
    {
        let Some(finished_at) = t.finished_at else {
            continue;
        };
        let window_closes = finished_at + crate::store::THIRTY_DAYS_SECS;
        let stale = match f.store.repair_cost_cache(t.id)? {
            Some((_, computed_at)) => computed_at < window_closes,
            None => true,
        };
        if !stale {
            continue;
        }
        let repair_cost = compute_repair_cost(f, &t, finished_at).await?;
        f.store.set_repair_cost_cache(t.id, repair_cost, now)?;
    }
    Ok(())
}

/// Multiset intersection: how many of `b`'s elements (with multiplicity)
/// also appear in `a`, each element of `a` usable at most once. Order-
/// independent (`min(count_a[x], count_b[x])` summed over every `x`).
fn multiset_overlap(a: &[(String, String)], b: &[(String, String)]) -> i64 {
    let mut counts: std::collections::HashMap<&(String, String), i64> =
        std::collections::HashMap::new();
    for x in a {
        *counts.entry(x).or_insert(0) += 1;
    }
    let mut overlap = 0i64;
    for y in b {
        if let Some(c) = counts.get_mut(y)
            && *c > 0
        {
            *c -= 1;
            overlap += 1;
        }
    }
    overlap
}

/// One landed task T's repair cost: for each later landing L on the same
/// repository within 30 days, the number of lines T's landing added (`T.
/// base_sha` to `T.landed_sha`) that L's landing removed or rewrote (`L.
/// base_sha` to `L.landed_sha`, the removed side — the same line-level
/// diff `compute_churn` reads), divided by all the lines L's landing
/// removed or rewrote, times L's total cost; summed over every L. A later
/// landing that rewrote none of T's lines contributes nothing. The
/// per-(T, L) git computation is cached in `line_overlap_cache`, keyed by
/// both landed commits, so it only ever runs once.
async fn compute_repair_cost(f: &Forge, t: &Task, finished_at: i64) -> Result<f64> {
    let repo = std::path::Path::new(&t.repo);
    let (t_added, _) = crate::git::diff_lines(repo, &t.base_sha, &t.landed_sha).await?;
    if t_added.is_empty() {
        return Ok(0.0);
    }
    let window_end = finished_at + crate::store::THIRTY_DAYS_SECS;
    let later = f
        .store
        .later_landings(&t.repo, t.id, finished_at, window_end)?;
    let mut total = 0.0;
    for l in later {
        let (overlap, removed_lines) =
            match f.store.line_overlap_cache(&t.landed_sha, &l.landed_sha)? {
                Some(pair) => pair,
                None => {
                    let (_, l_removed) =
                        crate::git::diff_lines(repo, &l.base_sha, &l.landed_sha).await?;
                    let overlap = multiset_overlap(&t_added, &l_removed);
                    let removed_lines = l_removed.len() as i64;
                    f.store.set_line_overlap_cache(
                        &t.landed_sha,
                        &l.landed_sha,
                        overlap,
                        removed_lines,
                    )?;
                    (overlap, removed_lines)
                }
            };
        if removed_lines == 0 {
            continue;
        }
        let fraction = overlap as f64 / removed_lines as f64;
        total += fraction * f.store.task_cost(l.id)?;
    }
    Ok(total)
}

/// Refresh `task_hand_commits` for every landed task that has never had it
/// computed: unlike `refresh_churn`/`refresh_repair_cost`, there is no
/// stale-until-a-window-closes schedule, since neither endpoint of the
/// range this counts (the previous landing's `landed_sha`, this task's own
/// `base_sha`) ever changes once the task has landed.
async fn refresh_hand_commits(f: &Forge) -> Result<()> {
    let now = crate::unix_now();
    for t in f
        .store
        .landed_tasks(&crate::store::StatsFilter::default())?
    {
        if f.store.hand_commits_cache(t.id)?.is_some() {
            continue;
        }
        let hand_commits = compute_hand_commits(f, &t).await?;
        f.store.set_hand_commits_cache(t.id, hand_commits, now)?;
    }
    Ok(())
}

/// One landed task's hand commits: commits not authored as Forge, on the
/// base branch, between the previous landing on the same repository
/// (`landed_sha`) and this task's own `base_sha` — the human attention a
/// person spent committing straight to the base while Forge was not
/// looking (docs/LATER.md, "Two metrics the record can compute and does
/// not"). Zero for the first landing a repository ever gets, since there
/// is no earlier landing to bound the range against.
async fn compute_hand_commits(f: &Forge, t: &Task) -> Result<i64> {
    let Some(prev) = f.store.previous_landing(&t.repo, t.id)? else {
        return Ok(0);
    };
    let repo = std::path::Path::new(&t.repo);
    crate::git::hand_commit_count(repo, &prev.landed_sha, &t.base_sha).await
}

/// `ttls` reduced to one `TimeToLiveRow` per workflow version present,
/// each its own median and 90th percentile.
fn time_to_live_rows(ttls: &[crate::store::TaskTtl]) -> Vec<TimeToLiveRow> {
    let mut groups: std::collections::BTreeMap<(String, String), Vec<i64>> =
        std::collections::BTreeMap::new();
    for t in ttls {
        groups
            .entry((t.workflow.clone(), t.hash.clone()))
            .or_default()
            .push(t.secs);
    }
    groups
        .into_iter()
        .map(|((workflow, hash), mut secs)| {
            let (median_secs, p90_secs) = median_p90(&mut secs);
            TimeToLiveRow {
                workflow,
                hash,
                n: secs.len() as i64,
                median_secs,
                p90_secs,
            }
        })
        .collect()
}

/// `ttls` reduced to one `TimeToLiveProjectRow` per project present, each
/// its own median and 90th percentile; a task with no project is left out,
/// same as `Store::project_stats`.
fn time_to_live_project_rows(ttls: &[crate::store::TaskTtl]) -> Vec<TimeToLiveProjectRow> {
    let mut groups: std::collections::BTreeMap<String, Vec<i64>> =
        std::collections::BTreeMap::new();
    for t in ttls {
        let Some(project) = &t.project else {
            continue;
        };
        groups.entry(project.clone()).or_default().push(t.secs);
    }
    groups
        .into_iter()
        .map(|(project, mut secs)| {
            let (median_secs, p90_secs) = median_p90(&mut secs);
            TimeToLiveProjectRow {
                project,
                n: secs.len() as i64,
                median_secs,
                p90_secs,
            }
        })
        .collect()
}

/// `days`, when given, is `forge stats --factors --days N`'s window: only
/// `factors` reads it, narrowed to tasks that finished in the last `days`
/// days; every other section reads this scope's whole history, as before.
pub async fn stats_doc(
    f: &Forge,
    scope: &crate::store::StatsFilter,
    days: Option<i64>,
) -> Result<StatsDoc> {
    refresh_churn(f).await?;
    refresh_hand_commits(f).await?;
    refresh_repair_cost(f).await?;
    let journal_stats = f.store.journal_control_stats()?;
    let journal = journal_stats
        .iter()
        .find(|j| j.has_journal)
        .map(Into::into)
        .unwrap_or_default();
    let no_journal = journal_stats
        .iter()
        .find(|j| !j.has_journal)
        .map(Into::into)
        .unwrap_or_default();
    let (projects, jobs) = if scope.project.is_none() && scope.initiative.is_none() {
        (
            f.store.project_stats()?.iter().map(Into::into).collect(),
            f.store
                .job_stats(crate::unix_now() - 86_400)?
                .iter()
                .map(Into::into)
                .collect(),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let human_attention_projects = if scope.project.is_none() && scope.initiative.is_none() {
        f.store
            .human_attention_project_stats()?
            .iter()
            .map(Into::into)
            .collect()
    } else {
        Vec::new()
    };
    let ttls = f.store.task_ttls(scope)?;
    let time_to_live_projects = if scope.project.is_none() && scope.initiative.is_none() {
        time_to_live_project_rows(&ttls)
    } else {
        Vec::new()
    };
    Ok(StatsDoc {
        workflows: f
            .store
            .workflow_stats(scope)?
            .iter()
            .map(Into::into)
            .collect(),
        steps: f.store.step_stats(scope)?.iter().map(Into::into).collect(),
        journal,
        no_journal,
        projects,
        jobs,
        by_role: f.store.role_stats()?.iter().map(Into::into).collect(),
        assessment_correlation: quality_correlation(f, scope)?,
        human_attention: f
            .store
            .human_attention_stats(scope)?
            .iter()
            .map(Into::into)
            .collect(),
        human_attention_projects,
        time_to_live: time_to_live_rows(&ttls),
        time_to_live_projects,
        factors: f
            .store
            .factor_stats(scope, days.map(|d| crate::unix_now() - d * 86_400))?
            .iter()
            .map(Into::into)
            .collect(),
        tools: None,
    })
}

/// One row of `forge plugin list` / `forge plugin list --json`: a plugin as
/// discovered, where it came from, and whether it is enabled.
#[derive(Serialize)]
pub struct PluginRow {
    pub name: String,
    pub description: String,
    pub dir: String,
    pub source: String,
    pub capabilities: Vec<String>,
    pub restart: String,
    pub enabled: bool,
}

impl From<&plugins::Plugin> for PluginRow {
    fn from(p: &plugins::Plugin) -> Self {
        PluginRow {
            name: p.name.clone(),
            description: p.manifest.description.clone(),
            dir: p.dir.display().to_string(),
            source: p.root.display().to_string(),
            capabilities: p
                .manifest
                .capabilities
                .iter()
                .map(|c| c.as_str().to_string())
                .collect(),
            restart: p.manifest.restart.as_str().to_string(),
            enabled: false,
        }
    }
}

/// One row of `forge plugin status` / `forge plugin status --json`: whether
/// a plugin is enabled and, per the supervisor's last record, whether it is
/// `running` (with `pid`/`uptime_secs`), `restarting` (with `restart_count`),
/// or `stopped` (with `last_exit`). A plugin no worker has ever supervised
/// reads as `stopped` with no `last_exit`.
#[derive(Serialize)]
pub struct PluginStatusRow {
    pub name: String,
    pub enabled: bool,
    pub state: String,
    pub pid: Option<i64>,
    pub uptime_secs: Option<i64>,
    pub restart_count: Option<u32>,
    pub last_exit: Option<String>,
}

impl PluginStatusRow {
    pub fn new(name: String, enabled: bool, run_state: &plugins::RunState) -> PluginStatusRow {
        let mut row = PluginStatusRow {
            name,
            enabled,
            state: String::new(),
            pid: None,
            uptime_secs: None,
            restart_count: None,
            last_exit: None,
        };
        match run_state {
            plugins::RunState::Running { pid, since } => {
                row.state = "running".into();
                row.pid = Some(*pid);
                row.uptime_secs = Some((crate::unix_now() - since).max(0));
            }
            plugins::RunState::Restarting { count } => {
                row.state = "restarting".into();
                row.restart_count = Some(*count);
            }
            plugins::RunState::Stopped { last_exit } => {
                row.state = "stopped".into();
                row.last_exit = last_exit.clone();
            }
        }
        row
    }
}

/// Every plugin found, in catalog order, with the store's enabled flag
/// merged in, plus the catalog's problems (a shadowed copy, a missing
/// configured root, a `plugin.toml` that failed to parse).
pub fn plugin_rows(f: &Forge) -> Result<(Vec<PluginRow>, Vec<Problem>)> {
    let home_cfg = config::load_home(&f.paths.home)?;
    let cat = plugins::load_catalog(&f.paths.home, &home_cfg.plugin_dirs);
    let enabled = f.store.enabled_plugins()?;
    let rows = cat
        .plugins
        .values()
        .map(|p| {
            let mut row = PluginRow::from(p);
            row.enabled = enabled.contains(&p.name);
            row
        })
        .collect();
    Ok((rows, cat.problems))
}

/// One repository listed under a project, and the paths it owns there
/// (`None` scope means the whole repository).
#[derive(Serialize)]
pub struct ProjectRepoRow {
    pub repo: String,
    pub scope: Option<String>,
}

impl From<&crate::store::ProjectRepo> for ProjectRepoRow {
    fn from(r: &crate::store::ProjectRepo) -> Self {
        ProjectRepoRow {
            repo: r.repo.clone(),
            scope: r.scope.clone(),
        }
    }
}

/// One row of `forge project list --json` / `forge project show --json`:
/// a project, its repositories and their scopes, task counts by state,
/// cost, and its own defaults (see docs/PROJECTS.md, "Defaults").
#[derive(Serialize)]
pub struct ProjectRow {
    pub name: String,
    pub purpose: String,
    pub created_at: i64,
    pub repos: Vec<ProjectRepoRow>,
    pub queued: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub unverified: i64,
    pub blocked: i64,
    pub withdrawn: i64,
    pub cost_usd: f64,
    /// Jobs (`kind = "run"` workflow runs) started in the last rolling
    /// 24h, counted separately from the task rollup above (docs/JOBS.md
    /// step 1d): every one of them, then how many of those reached each
    /// terminal state.
    pub jobs_today: i64,
    pub jobs_ok: i64,
    pub jobs_failed: i64,
    pub jobs_needs_human: i64,
    pub jobs_skipped: i64,
    pub workflow: Option<String>,
    pub per_task_usd: Option<f64>,
    pub per_initiative_usd: Option<f64>,
    pub supervisor_model: Option<String>,
    pub supervisor_per_lineage: Option<i64>,
    pub protected: Vec<String>,
    pub role_providers: std::collections::BTreeMap<String, String>,
    /// The escalator's proposals made on this project, newest first, and
    /// how each was answered (see docs/INTAKE.md, "The escalator").
    pub proposals: Vec<ProposalRow>,
}

/// A project's purpose as shown to a person: empty, rather than the
/// migration's placeholder, when nobody has set a real one yet (see
/// `crate::store::is_placeholder_purpose`).
fn real_purpose(purpose: &str) -> String {
    if crate::store::is_placeholder_purpose(purpose) {
        String::new()
    } else {
        purpose.to_string()
    }
}

pub fn project_row(f: &Forge, p: &crate::store::Project) -> Result<ProjectRow> {
    let repos = f
        .store
        .project_repos(&p.name)?
        .iter()
        .map(ProjectRepoRow::from)
        .collect();
    let cost = f.store.project_task_stats(&p.name)?.cost;
    let job_stats = f
        .store
        .project_job_stats(&p.name, crate::unix_now() - 86_400)?;
    let tasks = f.store.project_tasks(&p.name)?;
    let mut proposals: Vec<ProposalRow> = tasks.iter().filter_map(proposal_row).collect();
    proposals.sort_by(|a, b| b.task_id.cmp(&a.task_id));
    let mut stats = crate::store::ProjectTaskStats::default();
    for (t, _) in latest_per_lineage(f, &tasks)? {
        match t.state {
            TaskState::Queued => stats.queued += 1,
            TaskState::Running => stats.running += 1,
            TaskState::Succeeded => stats.succeeded += 1,
            TaskState::Failed => stats.failed += 1,
            TaskState::Unverified => stats.unverified += 1,
            TaskState::Blocked => stats.blocked += 1,
            TaskState::Withdrawn => stats.withdrawn += 1,
        }
    }
    Ok(ProjectRow {
        name: p.name.clone(),
        purpose: real_purpose(&p.purpose),
        created_at: p.created_at,
        repos,
        queued: stats.queued,
        running: stats.running,
        succeeded: stats.succeeded,
        failed: stats.failed,
        unverified: stats.unverified,
        blocked: stats.blocked,
        withdrawn: stats.withdrawn,
        cost_usd: cost,
        jobs_today: job_stats.today,
        jobs_ok: job_stats.ok,
        jobs_failed: job_stats.failed,
        jobs_needs_human: job_stats.needs_human,
        jobs_skipped: job_stats.skipped,
        workflow: p.workflow.clone(),
        per_task_usd: p.per_task_usd,
        per_initiative_usd: p.per_initiative_usd,
        supervisor_model: p.supervisor_model.clone(),
        supervisor_per_lineage: p.supervisor_per_lineage,
        protected: p.protected.clone().unwrap_or_default(),
        role_providers: p.role_providers.clone(),
        proposals,
    })
}

/// Every project, alphabetically, as `forge project list` shows it.
pub fn project_rows(f: &Forge) -> Result<Vec<ProjectRow>> {
    f.store
        .list_projects()?
        .iter()
        .map(|p| project_row(f, p))
        .collect()
}

/// The L0 rule name(s) a failed task's `reason` blames, in the exact form
/// `engine::l0_failure_reason` writes it ("L0 failed: has-commits",
/// optionally followed by " (after N attempt(s))"). `None` for any other
/// kind of failure: an agent failure, a budget cap, or a failing L1/L2
/// check never sets this prefix.
/// The L0 rules a failed task's reason names: "L0 failed: has-commits,
/// changes-match-git (after 2 attempt(s))" names two. A task can fail
/// several at once, and a streak on one of them must not be broken by a
/// failure that names it among others (initiative 2's third has-commits
/// failure also named changes-match-git and reset the count).
fn l0_rules_of(reason: &str) -> Vec<String> {
    let Some(rest) = reason.strip_prefix("L0 failed: ") else {
        return Vec::new();
    };
    let rest = rest.split(" (after").next().unwrap_or(rest).trim();
    rest.split(',')
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect()
}

/// The trailing run of failed tasks that all name one L0 rule, over
/// terminal tasks in the order they finished: the rule and the run's
/// length. Any terminal task that did not fail on an L0 rule ends the run.
pub(crate) fn same_rule_streak(terminal: &[(TaskState, &str)]) -> Option<(String, i64)> {
    let mut rule: Option<String> = None;
    let mut len = 0i64;
    for (state, reason) in terminal {
        let rules = if *state == TaskState::Failed {
            l0_rules_of(reason)
        } else {
            Vec::new()
        };
        if rules.is_empty() {
            rule = None;
            len = 0;
        } else if let Some(r) = &rule
            && rules.iter().any(|x| x == r)
        {
            len += 1;
        } else {
            rule = Some(rules[0].clone());
            len = 1;
        }
    }
    rule.map(|r| (r, len))
}

/// `tasks`, collapsed to one entry per lineage: a task and every task
/// that retries it, directly or through further retries, contribute only
/// their latest task (the one nothing in the lineage retries), since a
/// lineage's fate is its latest task's even though every task in it was
/// worked and paid for. Groups by `Store::root_of`, which walks the same
/// `retry_of` chain as `lineage_ids`. Paired with each latest task is how
/// many earlier tasks came before it in the lineage (its retry count).
fn latest_per_lineage(f: &Forge, tasks: &[Task]) -> Result<Vec<(Task, i64)>> {
    let mut groups: std::collections::BTreeMap<i64, Vec<Task>> = Default::default();
    for t in tasks {
        let root = f.store.root_of(t.id)?;
        groups.entry(root).or_default().push(t.clone());
    }
    Ok(groups
        .into_values()
        .map(|mut lineage| {
            lineage.sort_by_key(|t| t.id);
            let retries = (lineage.len() - 1) as i64;
            (lineage.pop().unwrap(), retries)
        })
        .collect())
}

/// One line, exactly as docs/PROJECTS.md, "State" derives it: "open"
/// while any task is queued or running; "held" when that is also true and
/// the worker is holding new claims for the initiative; "done with
/// failures" when none remain open and some failed; "done" otherwise.
pub fn initiative_state(tasks: &[Task], hold: Option<&str>) -> &'static str {
    let any_open = tasks.iter().any(|t| {
        !matches!(
            t.state,
            TaskState::Succeeded | TaskState::Failed | TaskState::Unverified | TaskState::Withdrawn
        )
    });
    if any_open {
        return if hold.is_some() { "held" } else { "open" };
    }
    if tasks.iter().any(|t| t.state == TaskState::Failed) {
        "done with failures"
    } else {
        "done"
    }
}

/// `initiative_hold`'s full answer: the short tag it exposes as
/// `held_rule` ("budget" or the rule name) paired with a human-readable
/// detail of the same fact (the amounts, or the rule and its streak) —
/// what `forge doctor`'s `initiatives` check reports, since `held_rule`
/// alone does not say how close or by how much.
fn initiative_hold_detail(
    f: &Forge,
    ini: &crate::store::Initiative,
) -> Result<Option<(String, String)>> {
    let budget = ini.budget_usd.or_else(|| {
        f.store
            .project(&ini.project)
            .ok()
            .flatten()
            .and_then(|p| p.per_initiative_usd)
    });
    if let Some(b) = budget {
        let spent = f.store.initiative_cost(ini.id)?;
        if spent >= b {
            return Ok(Some((
                "budget".to_string(),
                format!("budget: ${spent:.2} of ${b:.2}"),
            )));
        }
    }
    if ini.stop_after_same_rule <= 0 {
        return Ok(None);
    }
    let tasks = f.store.initiative_tasks(ini.id)?;
    let mut terminal: Vec<&Task> = tasks
        .iter()
        .filter(|t| {
            matches!(
                t.state,
                TaskState::Succeeded
                    | TaskState::Failed
                    | TaskState::Unverified
                    | TaskState::Withdrawn
            )
        })
        .collect();
    terminal.sort_by_key(|t| t.finished_at.unwrap_or(0));
    let seq: Vec<(TaskState, &str)> = terminal
        .iter()
        .map(|t| (t.state, t.reason.as_str()))
        .collect();
    Ok(same_rule_streak(&seq)
        .filter(|(_, len)| *len >= ini.stop_after_same_rule)
        .map(|(rule, len)| {
            let detail = format!("stop rule: {rule} (streak {len})");
            (rule, detail)
        }))
}

/// `Some` when the worker is currently holding new claims for this
/// initiative (see docs/PROJECTS.md, "Stop rule and budget"): its summed
/// cost has reached its budget (reported as `"budget"`), or its trailing
/// run of failed tasks all blame the same L0 rule and that run has
/// reached `stop_after_same_rule` (reported as that rule's name).
pub fn initiative_hold(f: &Forge, ini: &crate::store::Initiative) -> Result<Option<String>> {
    Ok(initiative_hold_detail(f, ini)?.map(|(tag, _)| tag))
}

/// `initiative_hold`'s detail message alone, for `forge doctor`'s
/// `initiatives` check: "budget: $spent of $cap" or "stop rule: <rule>
/// (streak <n>)".
pub(crate) fn initiative_hold_reason(
    f: &Forge,
    ini: &crate::store::Initiative,
) -> Result<Option<String>> {
    Ok(initiative_hold_detail(f, ini)?.map(|(_, detail)| detail))
}

/// Settle an initiative once every one of its tasks has reached a
/// terminal state (succeeded, failed, unverified or withdrawn): record
/// `settled_at` and emit `Event::InitiativeSettled`, tagged with
/// `task_id`, the task whose own change completed it (see
/// docs/PROJECTS.md, "One notification and one report"). A no-op once
/// already settled, or while the initiative still has open work.
pub fn maybe_settle_initiative(f: &Forge, task_id: i64, initiative_id: i64) -> Result<()> {
    let Some(ini) = f.store.initiative(initiative_id)? else {
        return Ok(());
    };
    if ini.settled_at.is_some() {
        return Ok(());
    }
    let tasks = f.store.initiative_tasks(initiative_id)?;
    let latest: Vec<Task> = latest_per_lineage(f, &tasks)?
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    let all_terminal = latest.iter().all(|t| {
        matches!(
            t.state,
            TaskState::Succeeded | TaskState::Failed | TaskState::Unverified | TaskState::Withdrawn
        )
    });
    if !all_terminal {
        return Ok(());
    }
    if f.store
        .settle_initiative(initiative_id, crate::unix_now())?
    {
        let cost = f.store.initiative_cost(initiative_id)?;
        let state = initiative_state(&latest, None).to_string();
        f.report.emit(
            task_id,
            crate::report::Event::InitiativeSettled {
                id: initiative_id,
                state: &state,
                cost,
            },
        );
    }
    Ok(())
}

/// One row of `forge initiative list` / `--json` and `forge initiative
/// show`: an initiative, its derived state, task counts by state, cost
/// and its own settings.
#[derive(Serialize)]
pub struct InitiativeRow {
    pub id: i64,
    pub project: String,
    pub outcome: String,
    pub state: String,
    pub held_rule: Option<String>,
    pub queued: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub unverified: i64,
    pub blocked: i64,
    pub withdrawn: i64,
    pub cost_usd: f64,
    pub budget_usd: Option<f64>,
    pub stop_after_same_rule: i64,
    pub created_at: i64,
    pub settled_at: Option<i64>,
}

pub fn initiative_row(f: &Forge, ini: &crate::store::Initiative) -> Result<InitiativeRow> {
    let tasks = f.store.initiative_tasks(ini.id)?;
    let latest: Vec<Task> = latest_per_lineage(f, &tasks)?
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    let hold = initiative_hold(f, ini)?;
    let state = initiative_state(&latest, hold.as_deref()).to_string();
    let mut stats = crate::store::ProjectTaskStats::default();
    for t in &latest {
        match t.state {
            TaskState::Queued => stats.queued += 1,
            TaskState::Running => stats.running += 1,
            TaskState::Succeeded => stats.succeeded += 1,
            TaskState::Failed => stats.failed += 1,
            TaskState::Unverified => stats.unverified += 1,
            TaskState::Blocked => stats.blocked += 1,
            TaskState::Withdrawn => stats.withdrawn += 1,
        }
    }
    Ok(InitiativeRow {
        id: ini.id,
        project: ini.project.clone(),
        outcome: ini.outcome.clone(),
        state,
        held_rule: hold,
        queued: stats.queued,
        running: stats.running,
        succeeded: stats.succeeded,
        failed: stats.failed,
        unverified: stats.unverified,
        blocked: stats.blocked,
        withdrawn: stats.withdrawn,
        cost_usd: f.store.initiative_cost(ini.id)?,
        budget_usd: ini.budget_usd,
        stop_after_same_rule: ini.stop_after_same_rule,
        created_at: ini.created_at,
        settled_at: ini.settled_at,
    })
}

/// Every initiative, oldest first; only `project`'s when given.
pub fn initiative_rows(f: &Forge, project: Option<&str>) -> Result<Vec<InitiativeRow>> {
    f.store
        .list_initiatives(project)?
        .iter()
        .map(|i| initiative_row(f, i))
        .collect()
}

/// One lineage in `InitiativeDoc.tasks`: its latest task's id, state and
/// reason, plus how many retries the lineage took to reach it.
#[derive(Serialize)]
pub struct InitiativeTaskRow {
    pub id: i64,
    pub state: String,
    pub reason: String,
    pub retries: i64,
    /// The assess directive's maintainability score for this task's own
    /// landing, 0-10; `None` when it never ran (workflow does not opt
    /// in, the task never landed, or the run failed; see
    /// docs/ACTIONS.md, "Assessment").
    pub score: Option<i64>,
    /// Total cost across every attempt of this lineage's latest task
    /// (`Store::task_cost`), so the report's task table can show what
    /// each one spent alongside the initiative's own total.
    pub cost_usd: f64,
}

/// One row of `InitiativeDoc.refused`: a verification rule name and how
/// many attempts of the initiative's tasks it refused.
#[derive(Serialize)]
pub struct RefusedRow {
    pub rule: String,
    pub count: i64,
}

/// One row of `InitiativeDoc.rulings`: a decision the supervisor made on
/// one of the initiative's tasks.
#[derive(Serialize)]
pub struct InitiativeRulingRow {
    pub task_id: i64,
    pub question: String,
    pub answer: String,
    pub citations: String,
}

/// One row of `InitiativeDoc.questions`: a question that reached the
/// operator, answered or (while the task is still blocked) not yet.
#[derive(Serialize)]
pub struct InitiativeQuestionRow {
    pub task_id: i64,
    pub question: String,
    pub answer: Option<String>,
}

/// One row of `InitiativeDoc.deployed`: a deploy one of the initiative's
/// tasks triggered on landing (see docs/DEPLOY.md, "When a deploy runs").
/// `findings` is `deploy-look`'s verdict, parsed from the deploy row's
/// `look_json`, empty when it never ran.
#[derive(Serialize)]
pub struct InitiativeDeployRow {
    pub task_id: i64,
    pub target: String,
    pub sha: String,
    pub check_ok: Option<bool>,
    pub rolled_back_to: Option<String>,
    pub findings: Vec<crate::deploy_look::Finding>,
}

/// How many attempts of `tasks` each verification rule refused, by name,
/// ordered by name.
fn refused_counts(f: &Forge, tasks: &[Task]) -> Result<Vec<RefusedRow>> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for t in tasks {
        for a in f.store.attempts(t.id)? {
            let rows: Vec<crate::checks::CheckResult> =
                serde_json::from_str(&a.verdict_json).unwrap_or_default();
            for c in rows.iter().filter(|c| !c.ok) {
                *counts.entry(c.name.clone()).or_default() += 1;
            }
        }
    }
    Ok(counts
        .into_iter()
        .map(|(rule, count)| RefusedRow { rule, count })
        .collect())
}

/// The generated report `forge initiative report` shows: the outcome,
/// each task and how it ended, what verification refused, what the
/// supervisor ruled, what reached the operator, cost and elapsed time
/// (see docs/PROJECTS.md, "One notification and one report").
#[derive(Serialize)]
pub struct InitiativeDoc {
    pub id: i64,
    pub project: String,
    pub outcome: String,
    pub state: String,
    pub held_rule: Option<String>,
    pub budget_usd: Option<f64>,
    pub stop_after_same_rule: i64,
    pub tasks: Vec<InitiativeTaskRow>,
    pub refused: Vec<RefusedRow>,
    pub rulings: Vec<InitiativeRulingRow>,
    pub questions: Vec<InitiativeQuestionRow>,
    pub deployed: Vec<InitiativeDeployRow>,
    pub cost_usd: f64,
    pub elapsed_secs: Option<i64>,
    pub created_at: i64,
    pub settled_at: Option<i64>,
    /// The escalator's proposal this initiative came from, when a "yes"
    /// answer filed it (see docs/INTAKE.md, "The escalator"); `None` for
    /// an initiative filed any other way.
    pub proposal: Option<ProposalRow>,
}

pub fn initiative_doc(f: &Forge, ini: &crate::store::Initiative) -> Result<InitiativeDoc> {
    let tasks = f.store.initiative_tasks(ini.id)?;
    let lineages = latest_per_lineage(f, &tasks)?;
    let latest: Vec<Task> = lineages.iter().map(|(t, _)| t.clone()).collect();
    let hold = initiative_hold(f, ini)?;
    let state = initiative_state(&latest, hold.as_deref()).to_string();
    let cost = f.store.initiative_cost(ini.id)?;
    let elapsed = tasks
        .iter()
        .filter_map(|t| t.finished_at)
        .max()
        .map(|end| end - ini.created_at);
    let decisions = f.store.decisions(&crate::store::DecisionFilter {
        initiative: Some(ini.id),
        ..Default::default()
    })?;
    // Scoped by initiative above, so a task-less admin decision (`forge
    // stats --reprice`, `task_id: None`) never reaches here; filter_map
    // just keeps that guarantee honest rather than unwrapping blindly.
    let rulings = decisions
        .iter()
        .filter(|d| d.answered_by == "supervisor")
        .filter_map(|d| {
            Some(InitiativeRulingRow {
                task_id: d.task_id?,
                question: d.question.clone(),
                answer: d.answer.clone(),
                citations: d.citations.clone(),
            })
        })
        .collect();
    let mut questions: Vec<InitiativeQuestionRow> = decisions
        .iter()
        .filter(|d| d.answered_by == "operator")
        .filter_map(|d| {
            Some(InitiativeQuestionRow {
                task_id: d.task_id?,
                question: d.question.clone(),
                answer: Some(d.answer.clone()),
            })
        })
        .collect();
    for t in tasks.iter().filter(|t| t.state == TaskState::Blocked) {
        if !questions.iter().any(|q| q.task_id == t.id) {
            let (_, text) = request_kind(&t.reason);
            questions.push(InitiativeQuestionRow {
                task_id: t.id,
                question: text,
                answer: None,
            });
        }
    }
    let proposal = f
        .store
        .project_tasks(&ini.project)?
        .iter()
        .find(|t| t.proposal_initiative == Some(ini.id))
        .and_then(proposal_row);
    let refused = refused_counts(f, &tasks)?;
    let mut deployed = Vec::new();
    for t in &tasks {
        for d in f.store.deploys_for_task(t.id)? {
            let findings = d
                .look_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default();
            deployed.push(InitiativeDeployRow {
                task_id: t.id,
                target: d.target,
                sha: d.sha,
                check_ok: d.check_ok,
                rolled_back_to: d.rolled_back_to,
                findings,
            });
        }
    }
    Ok(InitiativeDoc {
        id: ini.id,
        project: ini.project.clone(),
        outcome: ini.outcome.clone(),
        state,
        held_rule: hold,
        budget_usd: ini.budget_usd,
        stop_after_same_rule: ini.stop_after_same_rule,
        tasks: lineages
            .iter()
            .map(|(t, retries)| {
                Ok(InitiativeTaskRow {
                    id: t.id,
                    state: t.state.as_str().to_string(),
                    reason: t.reason.clone(),
                    retries: *retries,
                    score: f.store.assessment(t.id)?.map(|a| a.score),
                    cost_usd: f.store.task_cost(t.id)?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        refused,
        rulings,
        questions,
        deployed,
        cost_usd: cost,
        elapsed_secs: elapsed,
        created_at: ini.created_at,
        settled_at: ini.settled_at,
        proposal,
    })
}

/// The escalator's pattern (docs/INTAKE.md, "The escalator"), as
/// `forge ask` records it on a proposal placeholder's `proposal_json`:
/// the quoted requests that share a shape, why, and the outcome an
/// initiative would pursue if the operator says yes. Written by
/// `concierge::ask`, read back here to build `ProposalRow`.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProposalRecord {
    pub task_ids: Vec<i64>,
    pub repetition: String,
    pub outcome: String,
}

/// One row of a project's or an initiative's proposals: the escalator's
/// question, who it went to, and how it was answered (see
/// docs/INTAKE.md, "The escalator"). `answer` is `None` while the
/// placeholder task is still blocked; `initiative` is set only by a
/// "yes".
#[derive(Serialize)]
pub struct ProposalRow {
    pub task_id: i64,
    pub quoted: Vec<i64>,
    pub repetition: String,
    pub outcome: String,
    pub to: Option<String>,
    pub answer: Option<String>,
    pub initiative: Option<i64>,
}

/// Build a `ProposalRow` from a task the escalator blocked, or `None` for
/// any other task (`proposal_json` unset or unreadable).
pub fn proposal_row(t: &Task) -> Option<ProposalRow> {
    let raw = t.proposal_json.as_deref()?;
    let p: ProposalRecord = serde_json::from_str(raw).ok()?;
    Some(ProposalRow {
        task_id: t.id,
        quoted: p.task_ids,
        repetition: p.repetition,
        outcome: p.outcome,
        to: t.question_to.clone(),
        answer: t.proposal_answer.clone(),
        initiative: t.proposal_initiative,
    })
}

/// The brief an `intake` task's `interview` directive writes to `t.plan`
/// once its checklist is satisfied (see docs/INTAKE.md, "Mechanics").
#[derive(Debug, Deserialize)]
pub(crate) struct Brief {
    pub(crate) workflows: Vec<BriefWorkflow>,
    pub(crate) where_it_runs: String,
    #[serde(default)]
    pub(crate) confirmed: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BriefWorkflow {
    pub(crate) name: String,
    trigger: String,
    inputs: String,
    outputs: String,
    other_people: String,
    failure_today: String,
    success_signal: String,
    do_not_touch: String,
}

/// One workflow's fields, in the person's own words, as a paragraph: the
/// backlog entry `intake accept` files for it, the project's purpose (for
/// the first workflow named), and one entry of `PortalDoc.brief`.
pub(crate) fn workflow_paragraph(w: &BriefWorkflow) -> String {
    format!(
        "{}: starts when {}. Takes in {} and produces {}. Involves {}. Today, {}. Working would look like: {}. Must not change: {}.",
        w.name,
        w.trigger,
        w.inputs,
        w.outputs,
        w.other_people,
        w.failure_today,
        w.success_signal,
        w.do_not_touch,
    )
}

/// One deploy target on `PortalDoc`: the customer's "Running for you"
/// list (see docs/PORTAL.md, "What they see"). No method, args, check
/// command or repo path — those are how, not what.
#[derive(Serialize)]
pub struct PortalDeployTarget {
    pub name: String,
    pub where_it_runs: String,
    pub last_deployed_at: Option<i64>,
    pub check_ok: Option<bool>,
    pub look_ok: Option<bool>,
    pub screenshot: Option<String>,
}

/// One of a run workflow's last three jobs on `PortalDoc`, "Running for
/// you" continued (see docs/PORTAL.md): when it ran, whether it went
/// `"ok"`, `"failed"` or `"needs_human"` (`store::JobState::as_str()`),
/// and, on failure, a one-line reason cut from the first failing check's
/// tail. No job id, no cost, no trigger, no verdict rows — the
/// operator's `forge job show` carries those.
#[derive(Serialize)]
pub struct PortalJobRun {
    pub started_at: i64,
    pub state: String,
    pub reason: Option<String>,
}

/// One run workflow on `PortalDoc`: an automation this project's jobs
/// run through, and its last three jobs, newest first, from the same
/// job rows `forge job list` serves (see docs/PORTAL.md, "What they
/// see").
#[derive(Serialize)]
pub struct PortalWorkflow {
    pub name: String,
    pub jobs: Vec<PortalJobRun>,
}

/// One open initiative on `PortalDoc`: the customer's "Being built" list,
/// newest first, capped at ten (`PortalDoc.initiatives_more` the rest —
/// see docs/PORTAL.md). `state` is always one of "in progress" or
/// "waiting on you" — never the operator's `open`/`held` vocabulary.
/// `pieces` is how many tasks make up the initiative so far.
#[derive(Serialize)]
pub struct PortalInitiative {
    pub outcome: String,
    pub state: String,
    pub pieces: i64,
}

/// One open question on `PortalDoc`, addressed to the customer: the
/// "Needs you" list. `task_id` is what answering in place posts back
/// against (`forge answer <task_id> ...`); `asked_at` is Unix seconds, when
/// the task blocked on it.
#[derive(Serialize)]
pub struct PortalQuestion {
    pub task_id: i64,
    pub text: String,
    pub asked_at: i64,
}

/// One line on `PortalDoc`'s "Done" list, newest first, capped at ten
/// (`PortalDoc.landed_more` the rest — see docs/PORTAL.md): a landed
/// initiative's outcome sentence (`pieces` how many tasks it took), or a
/// landed task that belongs to no initiative (`pieces` `None`), `text`
/// its title if it has one, else a line derived from its request — never
/// the operator's full text, and never a path-like token.
#[derive(Serialize)]
pub struct PortalLanded {
    pub text: String,
    pub pieces: Option<i64>,
    pub landed_at: i64,
}

/// The confirmed intake brief on `PortalDoc`, in the person's own words
/// (see docs/INTAKE.md): "Your plan".
#[derive(Serialize)]
pub struct PortalBrief {
    pub where_it_runs: String,
    pub workflows: Vec<String>,
}

/// One open backlog item on `PortalDoc`: the rest of "Your plan", what
/// is queued but not yet running.
#[derive(Serialize)]
pub struct PortalBacklogItem {
    pub id: i64,
    pub text: String,
    pub created_at: i64,
}

/// The document `forge project view NAME --json` prints: everything the
/// customer portal's page needs for one project, in their own words (see
/// docs/PORTAL.md, "What they see"). No ids beyond a task's own (needed
/// to answer a question in place), no branches, no costs, no attempt
/// data, no verdict rows — the operator's page shows those; this shows
/// only what the customer asked for and what they're waiting on.
#[derive(Serialize)]
pub struct PortalDoc {
    pub project: String,
    /// The project's purpose, for the operator's own tools; never
    /// rendered on the customer's page (see docs/PORTAL.md).
    pub purpose: String,
    pub deploy_targets: Vec<PortalDeployTarget>,
    pub run_workflows: Vec<PortalWorkflow>,
    pub initiatives: Vec<PortalInitiative>,
    /// How many open initiatives past the ten in `initiatives` — "and n
    /// more" (0 when nothing was cut).
    pub initiatives_more: i64,
    pub questions: Vec<PortalQuestion>,
    pub landed: Vec<PortalLanded>,
    /// How many landed lines past the ten in `landed` — "and n more" (0
    /// when nothing was cut).
    pub landed_more: i64,
    pub brief: Option<PortalBrief>,
    pub backlog: Vec<PortalBacklogItem>,
}

pub fn portal_doc(f: &Forge, p: &crate::store::Project) -> Result<PortalDoc> {
    let mut deploy_targets = Vec::new();
    for t in f.store.deploy_targets(&p.name)? {
        let last = f.store.deploys(&p.name, Some(&t.name))?.into_iter().next();
        let (last_deployed_at, check_ok, look_ok, screenshot) = match &last {
            Some(d) => (
                Some(d.started_at),
                d.check_ok,
                d.look_ok,
                d.smoke_json.as_ref().map(|_| {
                    f.paths
                        .home
                        .join("deploys")
                        .join(d.id.to_string())
                        .join("screenshot.png")
                        .display()
                        .to_string()
                }),
            ),
            None => (None, None, None, None),
        };
        deploy_targets.push(PortalDeployTarget {
            name: t.name,
            where_it_runs: t.args.get("host").cloned().unwrap_or_default(),
            last_deployed_at,
            check_ok,
            look_ok,
            screenshot,
        });
    }

    // Running for you, continued: every run workflow this project's jobs
    // have used, newest job first, each capped at its last three (see
    // docs/PORTAL.md). `f.store.jobs` already orders newest first, so a
    // single pass building each workflow's entry in first-seen order
    // keeps both the workflow order and each one's job order correct.
    let mut run_workflows: Vec<PortalWorkflow> = Vec::new();
    for j in f.store.jobs(Some(&p.name), None)? {
        let entry = match run_workflows.iter().position(|w| w.name == j.workflow) {
            Some(i) => &mut run_workflows[i],
            None => {
                run_workflows.push(PortalWorkflow {
                    name: j.workflow.clone(),
                    jobs: Vec::new(),
                });
                run_workflows.last_mut().expect("just pushed")
            }
        };
        if entry.jobs.len() < 3 {
            entry.jobs.push(PortalJobRun {
                started_at: j.started_at,
                state: j.state.as_str().to_string(),
                reason: job_reason(&j),
            });
        }
    }

    let tasks = f.store.project_tasks(&p.name)?;
    let latest: Vec<Task> = latest_per_lineage(f, &tasks)?
        .into_iter()
        .map(|(t, _)| t)
        .collect();

    // A blocked task whose kind is "question" is what the customer sees
    // under Needs you; it also flips its own initiative's plain state to
    // "waiting on you" below, the two lists staying consistent with each
    // other by construction.
    let mut questions = Vec::new();
    let mut questioning_initiatives: std::collections::BTreeSet<i64> = Default::default();
    for t in latest.iter().filter(|t| t.state == TaskState::Blocked) {
        let (kind, text) = request_kind(&t.reason);
        if kind == "question" {
            questions.push(PortalQuestion {
                task_id: t.id,
                text,
                asked_at: t.finished_at.unwrap_or(t.created_at),
            });
            if let Some(ini) = t.initiative {
                questioning_initiatives.insert(ini);
            }
        }
    }

    let all_initiatives = initiative_rows(f, Some(&p.name))?;

    // Being built: every open initiative (never settled), newest first,
    // capped at ten.
    let mut open: Vec<&InitiativeRow> = all_initiatives
        .iter()
        .filter(|r| r.settled_at.is_none())
        .collect();
    open.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let initiatives_total = open.len();
    let initiatives: Vec<PortalInitiative> = open
        .into_iter()
        .take(10)
        .map(|r| {
            let state = if questioning_initiatives.contains(&r.id) {
                "waiting on you"
            } else {
                "in progress"
            };
            PortalInitiative {
                outcome: r.outcome.clone(),
                state: state.to_string(),
                pieces: initiative_pieces(r),
            }
        })
        .collect();
    let initiatives_more = (initiatives_total - initiatives.len()) as i64;

    // Done: a landed initiative is one line, its outcome and how many
    // tasks it took; a landed task belonging to no initiative is one
    // line, its title or a line derived from its request. Merged, newest
    // first, capped at ten.
    let mut landed: Vec<PortalLanded> = Vec::new();
    for r in all_initiatives.iter().filter(|r| r.settled_at.is_some()) {
        landed.push(PortalLanded {
            text: r.outcome.clone(),
            pieces: Some(initiative_pieces(r)),
            landed_at: r.settled_at.unwrap_or(0),
        });
    }
    for t in latest
        .iter()
        .filter(|t| !t.landed_sha.is_empty() && t.initiative.is_none())
    {
        landed.push(PortalLanded {
            text: crate::render::landed_task_line(t),
            pieces: None,
            landed_at: t.finished_at.unwrap_or(0),
        });
    }
    landed.sort_by(|a, b| b.landed_at.cmp(&a.landed_at));
    let landed_total = landed.len();
    landed.truncate(10);
    let landed_more = (landed_total - landed.len()) as i64;

    // The confirmed brief lives only on the intake task that produced it
    // (`t.plan`, see docs/INTAKE.md); a project carries no copy of its
    // own, so the most recent confirmed one is re-read here.
    let brief = tasks
        .iter()
        .filter(|t| t.workflow == "intake" && !t.plan.is_empty())
        .filter_map(|t| serde_json::from_str::<Brief>(&t.plan).ok())
        .rfind(|b| b.confirmed)
        .map(|b| PortalBrief {
            where_it_runs: b.where_it_runs,
            workflows: b.workflows.iter().map(workflow_paragraph).collect(),
        });

    let backlog = f
        .store
        .backlog(&p.name)?
        .into_iter()
        .filter(|b| b.done_at.is_none())
        .map(|b| PortalBacklogItem {
            id: b.id,
            text: b.text,
            created_at: b.created_at,
        })
        .collect();

    Ok(PortalDoc {
        project: p.name.clone(),
        purpose: real_purpose(&p.purpose),
        deploy_targets,
        run_workflows,
        initiatives,
        initiatives_more,
        questions,
        landed,
        landed_more,
        brief,
        backlog,
    })
}

/// A failed, needs-human, or skipped job's one-line reason on `PortalDoc`:
/// for a failure, the first line of the first failing check's tail in
/// `verdict_json`; for `Skipped`, the first line of the `[skip_if]`
/// command's own tail — its stdout's first line, recorded there by
/// `job::run_now` (docs/JOBS.md, "Skipping a run"). Either way, any
/// path-like token is stripped and the result cut at 120 characters on a
/// word boundary, the same treatment `derive_landed_line` gives a landed
/// task's own request text (see docs/PORTAL.md). `None` for a job that is
/// queued, running, dropped, or went ok, or whose verdict carries no
/// matching check.
fn job_reason(j: &crate::store::Job) -> Option<String> {
    use crate::store::JobState;
    let verdict: Vec<crate::checks::CheckResult> =
        serde_json::from_str(&j.verdict_json).unwrap_or_default();
    let tail = match j.state {
        JobState::Skipped => &verdict.first()?.tail,
        JobState::Failed | JobState::NeedsHuman => &verdict.iter().find(|c| !c.ok)?.tail,
        _ => return None,
    };
    let line = tail.lines().next().unwrap_or(tail);
    let stripped = crate::render::strip_path_like_tokens(line.trim());
    Some(crate::render::truncate_at_word_boundary(
        stripped.trim(),
        120,
    ))
}

/// How many tasks make up an initiative so far — every lineage, whatever
/// state it's in — the "n pieces of work" beside its outcome on
/// `PortalDoc` (see docs/PORTAL.md).
fn initiative_pieces(r: &InitiativeRow) -> i64 {
    r.queued + r.running + r.succeeded + r.failed + r.unverified + r.blocked + r.withdrawn
}

/// The document `forge job show ID --json` prints: one job with every
/// step and effect it recorded, newest-run fields alongside them.
#[derive(Serialize)]
pub struct JobDoc {
    pub id: i64,
    pub project: String,
    pub workflow: String,
    pub workflow_hash: String,
    pub landed_sha: String,
    pub trigger_kind: String,
    pub trigger_ref: String,
    pub state: String,
    /// `"repo"` or `"catalog"` (see `store::Job::workflow_source`).
    pub workflow_source: String,
    pub dry_run: bool,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub cost_usd: Option<f64>,
    pub verdict_json: String,
    /// When this job becomes claimable, a unix second; `None` for a job
    /// that was never delayed (see `store::Job::due_at`).
    pub due_at: Option<i64>,
    /// How many times `[limits] on_failure = "retry:N"` has already
    /// requeued this job's lineage (see `store::Job::retry_count`).
    pub retry_count: i64,
    pub steps: Vec<crate::store::JobStep>,
    pub effects: Vec<crate::store::JobEffect>,
}

pub fn job_doc(f: &Forge, j: &crate::store::Job) -> Result<JobDoc> {
    let steps = f.store.job_steps(j.id)?;
    let effects = f.store.job_effects(j.id)?;
    Ok(JobDoc {
        id: j.id,
        project: j.project.clone(),
        workflow: j.workflow.clone(),
        workflow_hash: j.workflow_hash.clone(),
        landed_sha: j.landed_sha.clone(),
        trigger_kind: j.trigger_kind.clone(),
        trigger_ref: j.trigger_ref.clone(),
        state: j.state.as_str().to_string(),
        workflow_source: j.workflow_source.clone(),
        dry_run: j.dry_run,
        started_at: j.started_at,
        finished_at: j.finished_at,
        cost_usd: j.cost_usd,
        verdict_json: j.verdict_json.clone(),
        due_at: j.due_at,
        retry_count: j.retry_count,
        steps,
        effects,
    })
}

#[cfg(test)]
mod stats_tests {
    use super::*;
    use crate::store::{Assessment, Attempt, AttemptState, FinishAttempt, StepStat, WorkflowStat};

    #[test]
    fn workflow_row_carries_named_fields_and_the_deprecated_legacy_keys() {
        let w = WorkflowStat {
            workflow: "direct".into(),
            hash: "abc123".into(),
            tasks: 1,
            succeeded: 1,
            failed: 0,
            blocked: 0,
            unverified: 0,
            cost: 2.0,
            attempts: 1,
            landed: 0,
            broke_base: 0,
            repaired: 0,
            repair_cost: 0.0,
            added_lines: 0,
            churned_lines: 0,
        };
        let row = StatsWorkflowRow::from(&w);
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["workflow"], "direct");
        assert_eq!(v["pieces"], 1);
        assert_eq!(v["mean_cost_usd"], 2.0);
        assert_eq!(v["cost_per_success_usd"], 2.0);
        assert!(v["cost_per_landed_usd"].is_null());
        assert_eq!(v["broke_base"], 0);
        assert!(v["broke_base_share"].is_null(), "nothing landed");
        assert_eq!(v["repaired"], 0);
        assert!(v["repaired_share"].is_null(), "nothing landed");
        assert!(v["true_cost_per_landed_usd"].is_null(), "nothing landed");
        assert!(v["churn_share"].is_null(), "nothing added yet");
        // Deprecated header-named keys stay present, flattened alongside.
        assert_eq!(v["WF"], "direct");
        assert_eq!(v["TASKS"], 1);
        assert_eq!(v["$/OK"], 2.0);
        assert!(v["$/LANDED"].is_null());
    }

    #[test]
    fn workflow_row_shares_defect_escape_over_landed() {
        let w = WorkflowStat {
            workflow: "direct".into(),
            hash: "abc123".into(),
            tasks: 4,
            succeeded: 4,
            failed: 0,
            blocked: 0,
            unverified: 0,
            cost: 4.0,
            attempts: 4,
            landed: 4,
            broke_base: 1,
            repaired: 2,
            repair_cost: 2.0,
            added_lines: 20,
            churned_lines: 5,
        };
        let row = StatsWorkflowRow::from(&w);
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["broke_base"], 1);
        assert_eq!(v["broke_base_share"], 0.25);
        assert_eq!(v["repaired"], 2);
        assert_eq!(v["repaired_share"], 0.5);
        assert_eq!(v["repair_cost_usd"], 2.0);
        assert_eq!(v["true_cost_per_landed_usd"], 1.5, "(4.0 + 2.0) / 4");
        assert_eq!(v["churn_share"], 0.25, "5 / 20");
    }

    fn fixture() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = crate::ctx::Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = crate::store::Store::open(&paths.home.join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        (dir, f)
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(dir.path())
            .status()
            .unwrap();
        dir
    }

    /// Three landings on the same fixture repository, the second rewriting
    /// the only line the first added: `task_churn`'s worked example (see
    /// docs/LATER.md, the delayed-cost follow-up to "Defect escape").
    #[tokio::test]
    async fn churn_measures_lines_a_later_landing_on_the_same_repo_rewrites() {
        let (_home, f) = fixture();
        let repo_dir = init_repo();
        let repo = repo_dir.path();

        std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let c0 = crate::git::commit_all(repo, "base").await.unwrap().unwrap();
        std::fs::write(repo.join("a.txt"), "one\nALPHA\nthree\n").unwrap();
        let c1 = crate::git::commit_all(repo, "t1").await.unwrap().unwrap();
        std::fs::write(repo.join("a.txt"), "one\nBETA\nthree\n").unwrap();
        let c2 = crate::git::commit_all(repo, "t2").await.unwrap().unwrap();
        std::fs::write(repo.join("a.txt"), "one\nBETA\nthree\nfour\n").unwrap();
        let c3 = crate::git::commit_all(repo, "t3").await.unwrap().unwrap();

        let repo_s = repo.to_string_lossy().to_string();
        let landing = |base: &str, landed: &str, finished_at: i64| Task {
            repo: repo_s.clone(),
            task: "t".into(),
            base_branch: "main".into(),
            base_sha: base.to_string(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: finished_at,
            started_at: Some(finished_at),
            finished_at: Some(finished_at),
            workflow: "direct".into(),
            landed_sha: landed.to_string(),
            ..Default::default()
        };
        let insert = |mut t: Task| {
            t.id = f.store.insert_task(&t).unwrap();
            f.store.update_task(&t).unwrap();
            t
        };

        let t1 = insert(landing(&c0, &c1, 1000));
        let t2 = insert(landing(&c1, &c2, 2000));
        let t3 = insert(landing(&c2, &c3, 3000));

        refresh_churn(&f).await.unwrap();

        assert_eq!(
            f.store.churn_cache(t1.id).unwrap().map(|(a, c, _)| (a, c)),
            Some((1, 1)),
            "T2 rewrote the only line T1 added"
        );
        assert_eq!(
            f.store.churn_cache(t2.id).unwrap().map(|(a, c, _)| (a, c)),
            Some((1, 0)),
            "T3 left BETA alone"
        );
        assert_eq!(
            f.store.churn_cache(t3.id).unwrap().map(|(a, c, _)| (a, c)),
            Some((1, 0)),
            "nothing has landed after T3 yet"
        );

        let raw = f
            .store
            .workflow_stats(&crate::store::StatsFilter::default())
            .unwrap();
        let stat = raw.iter().find(|w| w.workflow == "direct").unwrap();
        assert_eq!(stat.added_lines, 3);
        assert_eq!(stat.churned_lines, 1);

        let doc = stats_doc(&f, &crate::store::StatsFilter::default(), None)
            .await
            .unwrap();
        let w = doc
            .workflows
            .iter()
            .find(|w| w.workflow == "direct")
            .unwrap();
        assert_eq!(w.churn_share, Some(1.0 / 3.0));
    }

    /// Task 357's fixture repository, replayed for the replacement metric:
    /// path overlap over-counted every task, since src/cli.rs-style files
    /// are touched by nearly every landing. Line overlap does not: a later
    /// landing that rewrites half of an earlier one's added lines charges
    /// it half its cost, and one that only touches the same file without
    /// rewriting any of its lines charges it nothing.
    #[tokio::test]
    async fn repair_cost_attributes_a_later_landings_cost_by_the_share_of_lines_it_rewrote() {
        let (_home, f) = fixture();
        let repo_dir = init_repo();
        let repo = repo_dir.path();

        std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let c0 = crate::git::commit_all(repo, "base").await.unwrap().unwrap();
        // T adds two lines (ALPHA, BETA), removing "two".
        std::fs::write(repo.join("a.txt"), "one\nALPHA\nBETA\nthree\n").unwrap();
        let c1 = crate::git::commit_all(repo, "t").await.unwrap().unwrap();
        // L1 rewrites one of T's two added lines (ALPHA -> GAMMA) and also
        // drops "three", a line T never touched: of the two lines L1's
        // landing removed or rewrote, only one was T's.
        std::fs::write(repo.join("a.txt"), "one\nGAMMA\nBETA\n").unwrap();
        let c2 = crate::git::commit_all(repo, "l1").await.unwrap().unwrap();
        // L2 touches a.txt again but only rewrites a line neither T nor L1
        // added ("one" -> "ONE"): none of T's lines.
        std::fs::write(repo.join("a.txt"), "ONE\nGAMMA\nBETA\n").unwrap();
        let c3 = crate::git::commit_all(repo, "l2").await.unwrap().unwrap();

        let repo_s = repo.to_string_lossy().to_string();
        let landing = |base: &str, landed: &str, finished_at: i64| Task {
            repo: repo_s.clone(),
            task: "t".into(),
            base_branch: "main".into(),
            base_sha: base.to_string(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: finished_at,
            started_at: Some(finished_at),
            finished_at: Some(finished_at),
            workflow: "direct".into(),
            landed_sha: landed.to_string(),
            ..Default::default()
        };
        let insert = |mut t: Task| {
            t.id = f.store.insert_task(&t).unwrap();
            f.store.update_task(&t).unwrap();
            t
        };
        let cost = |task_id, amount: f64| {
            let attempt_id = f
                .store
                .insert_attempt(&Attempt {
                    task_id,
                    attempt_no: 1,
                    step: "code".into(),
                    started_at: 0,
                    ..Default::default()
                })
                .unwrap();
            f.store
                .finish_attempt(&FinishAttempt {
                    id: attempt_id,
                    state: AttemptState::Succeeded,
                    reason: String::new(),
                    finished_at: Some(1),
                    agent_exit: Some(0),
                    timed_out: false,
                    num_turns: 1,
                    tool_calls: 1,
                    cost_usd: Some(amount),
                    agent_ms: 0,
                    commits: 1,
                    files_changed: 1,
                    dirty: false,
                    verdict_json: "[]".into(),
                    result_text: String::new(),
                    envelope_json: String::new(),
                    rl_five_hour: None,
                    rl_seven_day: None,
                    rl_five_hour_resets: None,
                    rl_seven_day_resets: None,
                    end_sha: String::new(),
                    outputs_json: String::new(),
                    session_id: String::new(),
                    first_edit: None,
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                    early_signals: "[]".into(),
                    early_near: "[]".into(),
                })
                .unwrap();
        };

        let t = insert(landing(&c0, &c1, 1000));
        let l1 = insert(landing(&c1, &c2, 2000));
        let l2 = insert(landing(&c2, &c3, 3000));
        cost(l1.id, 10.0);
        cost(l2.id, 7.0);

        refresh_repair_cost(&f).await.unwrap();

        assert_eq!(
            f.store
                .line_overlap_cache(&t.landed_sha, &l1.landed_sha)
                .unwrap(),
            Some((1, 2)),
            "L1 removed or rewrote ALPHA and three; only ALPHA was T's"
        );
        assert_eq!(
            f.store
                .line_overlap_cache(&t.landed_sha, &l2.landed_sha)
                .unwrap(),
            Some((0, 1)),
            "L2 removed \"one\", which was never T's"
        );
        assert_eq!(
            f.store
                .repair_cost_cache(t.id)
                .unwrap()
                .map(|(cost, _)| cost),
            Some(5.0),
            "half of L1's $10 (1/2 of its rewritten lines were T's) plus none of L2's $7"
        );

        let raw = f
            .store
            .workflow_stats(&crate::store::StatsFilter::default())
            .unwrap();
        let stat = raw.iter().find(|w| w.workflow == "direct").unwrap();
        assert_eq!(stat.repair_cost, 5.0);
    }

    /// Six landed, assessed tasks whose scores rise from 3 to 8 as their
    /// churn share falls from 1.0 to 0.0: the assess directive's fast
    /// proxy tracking the delayed-cost measure it stands in for (see
    /// docs/ACTIONS.md, "Assessment"). Sets the churn and repair-cost
    /// caches directly, past their 30-day window, so `quality_correlation`
    /// reads cached numbers rather than exercising `refresh_churn`'s git
    /// diff (already covered above).
    #[test]
    fn quality_correlation_is_negative_when_score_rises_as_churn_falls() {
        let (_home, f) = fixture();
        let landing = |finished_at: i64| Task {
            repo: "/does/not/matter".into(),
            task: "t".into(),
            base_branch: "main".into(),
            base_sha: "base".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: finished_at,
            started_at: Some(finished_at),
            finished_at: Some(finished_at),
            workflow: "direct".into(),
            landed_sha: format!("landed-{finished_at}"),
            ..Default::default()
        };
        for (i, (score, added, churned)) in [
            (3i64, 10i64, 10i64),
            (4, 10, 8),
            (5, 10, 6),
            (6, 10, 4),
            (7, 10, 2),
            (8, 10, 0),
        ]
        .into_iter()
        .enumerate()
        {
            let mut t = landing(1000 + i as i64);
            t.id = f.store.insert_task(&t).unwrap();
            f.store.update_task(&t).unwrap();
            f.store
                .insert_assessment(&Assessment {
                    id: 0,
                    task_id: t.id,
                    score,
                    findings_json: "[]".into(),
                    model: "m".into(),
                    provider: "p".into(),
                    cost_usd: None,
                    created_at: t.finished_at.unwrap(),
                })
                .unwrap();
            let computed_at = t.finished_at.unwrap() + crate::store::THIRTY_DAYS_SECS;
            f.store
                .set_churn_cache(t.id, added, churned, computed_at)
                .unwrap();
            f.store
                .set_repair_cost_cache(t.id, 0.0, computed_at)
                .unwrap();
        }

        let rows = quality_correlation(&f, &crate::store::StatsFilter::default()).unwrap();
        let churn = rows.iter().find(|r| r.measure == "churn").unwrap();
        assert_eq!(churn.n, 6);
        assert!(
            churn.rho.unwrap() < 0.0,
            "score rises as churn falls, expected a negative rho: {:?}",
            churn.rho
        );
    }

    #[test]
    fn step_row_carries_named_fields_and_the_deprecated_legacy_keys() {
        let st = StepStat {
            workflow: "direct".into(),
            step: "code".into(),
            attempts: 1,
            succeeded: 1,
            agent_failed: 0,
            checks_failed: 0,
            needs_input: 0,
            mean_turns: 3.0,
            cost: 2.0,
            mean_ms: 4000.0,
            mean_first_edit: Some(1.5),
            mean_input_tokens: None,
        };
        let row = StatsStepRow::from(&st);
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["step"], "code");
        assert_eq!(v["mean_secs"], 4.0);
        assert_eq!(v["mean_first_edit"], 1.5);
        assert!(v["mean_input_tokens"].is_null());
        assert_eq!(v["STEP"], "code");
        assert_eq!(v["SECS"], 4.0);
        assert_eq!(v["EDIT@"], 1.5);
        assert!(v["TOKENS"].is_null());
    }

    #[test]
    fn stats_doc_omits_tools_when_not_requested() {
        let doc = StatsDoc {
            workflows: vec![],
            steps: vec![],
            journal: StatsJournalRow::default(),
            no_journal: StatsJournalRow::default(),
            projects: vec![],
            jobs: vec![],
            by_role: vec![],
            assessment_correlation: vec![],
            human_attention: vec![],
            human_attention_projects: vec![],
            time_to_live: vec![],
            time_to_live_projects: vec![],
            factors: vec![],
            tools: None,
        };
        let v = serde_json::to_value(&doc).unwrap();
        assert!(v.get("tools").is_none(), "{v}");
        assert!(v.get("projects").is_none(), "{v}");
        assert!(v.get("jobs").is_none(), "{v}");
        assert!(v.get("human_attention_projects").is_none(), "{v}");
        assert!(v.get("time_to_live_projects").is_none(), "{v}");
    }

    #[test]
    fn journal_row_computes_succeeded_share_and_carries_options_through() {
        let j = JournalStat {
            has_journal: true,
            attempts: 4,
            succeeded: 3,
            mean_turns: 25.0,
            mean_first_edit: Some(8.0),
            mean_cost_usd: 0.75,
        };
        let row = StatsJournalRow::from(&j);
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["attempts"], 4);
        assert_eq!(v["succeeded"], 3);
        assert_eq!(v["succeeded_share"], 0.75);
        assert_eq!(v["mean_turns"], 25.0);
        assert_eq!(v["mean_first_edit"], 8.0);
        assert_eq!(v["mean_cost_usd"], 0.75);

        let empty = StatsJournalRow::default();
        let v = serde_json::to_value(&empty).unwrap();
        assert!(v["succeeded_share"].is_null());
        assert!(v["mean_first_edit"].is_null());
    }
}

#[cfg(test)]
mod lineage_rollup_tests {
    use super::*;
    use crate::ctx::Paths;
    use crate::store::{Initiative, Project, Store};

    /// A `Forge` over a fresh, empty store in a throwaway home.
    fn fixture() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        (dir, f)
    }

    fn fixture_task(
        project: &str,
        state: TaskState,
        retry_of: Option<i64>,
        initiative: i64,
    ) -> Task {
        Task {
            repo: "/repo".into(),
            task: "do the thing".into(),
            base_branch: "main".into(),
            model: "sonnet".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state,
            created_at: crate::unix_now(),
            workflow: "direct".into(),
            project: Some(project.into()),
            initiative: Some(initiative),
            retry_of,
            ..Default::default()
        }
    }

    fn insert(f: &Forge, mut t: Task) -> Task {
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();
        t
    }

    /// A lineage of three (blocked, then failed, then a retry that
    /// landed) must count once, as its latest task's state: succeeded,
    /// not also blocked and failed. The report names the same lineage by
    /// its latest task and says how many retries it took to land.
    #[test]
    fn a_landed_retry_counts_its_lineage_once_as_succeeded() {
        let (_dir, f) = fixture();
        f.store
            .create_project(&Project {
                name: "demo".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        let ini_id = f
            .store
            .create_initiative(&Initiative {
                project: "demo".into(),
                outcome: "o".into(),
                stop_after_same_rule: 3,
                created_at: 1,
                ..Default::default()
            })
            .unwrap();

        let t1 = insert(&f, fixture_task("demo", TaskState::Blocked, None, ini_id));
        let t2 = insert(
            &f,
            fixture_task("demo", TaskState::Failed, Some(t1.id), ini_id),
        );
        let mut t3 = fixture_task("demo", TaskState::Succeeded, Some(t2.id), ini_id);
        t3.finished_at = Some(crate::unix_now());
        let t3 = insert(&f, t3);

        let ini = f.store.initiative(ini_id).unwrap().unwrap();
        let row = initiative_row(&f, &ini).unwrap();
        assert_eq!(row.succeeded, 1, "the lineage's latest task landed");
        assert_eq!(row.blocked, 0);
        assert_eq!(row.failed, 0);
        assert_eq!(row.state, "done");

        let doc = initiative_doc(&f, &ini).unwrap();
        assert_eq!(doc.tasks.len(), 1);
        assert_eq!(doc.tasks[0].id, t3.id);
        assert_eq!(doc.tasks[0].state, "succeeded");
        assert_eq!(doc.tasks[0].retries, 2, "it took two retries to land");
    }

    /// `forge project show`'s task counts collapse the same way: a
    /// project-scoped task that landed on retry counts once, as succeeded.
    #[test]
    fn project_show_counts_the_same_lineage_once() {
        let (_dir, f) = fixture();
        f.store
            .create_project(&Project {
                name: "demo".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        // No initiative: these are standalone project tasks.
        let t1 = insert(
            &f,
            Task {
                initiative: None,
                ..fixture_task("demo", TaskState::Blocked, None, 0)
            },
        );
        insert(
            &f,
            Task {
                initiative: None,
                ..fixture_task("demo", TaskState::Succeeded, Some(t1.id), 0)
            },
        );

        let p = f.store.project("demo").unwrap().unwrap();
        let row = project_row(&f, &p).unwrap();
        assert_eq!(row.succeeded, 1);
        assert_eq!(row.blocked, 0);
    }
}

#[cfg(test)]
mod stop_rule_tests {
    use crate::store::TaskState::{Failed, Succeeded};

    #[test]
    fn a_streak_survives_a_failure_that_names_the_rule_among_others() {
        let seq = [
            (Failed, "L0 failed: has-commits (after 2 attempt(s))"),
            (Failed, "L0 failed: has-commits (after 2 attempt(s))"),
            (
                Failed,
                "L0 failed: has-commits, changes-match-git (after 2 attempt(s))",
            ),
        ];
        assert_eq!(
            super::same_rule_streak(&seq),
            Some(("has-commits".to_string(), 3))
        );
    }

    #[test]
    fn a_landing_or_a_different_rule_ends_the_streak() {
        let broken = [
            (Failed, "L0 failed: has-commits (after 2 attempt(s))"),
            (Succeeded, "landed main @ abc"),
            (Failed, "L0 failed: has-commits (after 2 attempt(s))"),
        ];
        assert_eq!(
            super::same_rule_streak(&broken),
            Some(("has-commits".to_string(), 1))
        );
        let other = [
            (Failed, "L0 failed: clean-tree (after 1 attempt(s))"),
            (Failed, "L0 failed: has-commits (after 1 attempt(s))"),
        ];
        assert_eq!(
            super::same_rule_streak(&other),
            Some(("has-commits".to_string(), 1))
        );
    }
}

#[cfg(test)]
mod portal_tests {
    use super::*;
    use crate::ctx::Paths;
    use crate::store::{Attempt, AttemptState, DeployTarget, Initiative, Project, Store};
    use std::collections::BTreeMap;

    fn fixture() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        (dir, f)
    }

    fn insert(f: &Forge, mut t: Task) -> Task {
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();
        t
    }

    /// Every word docs/PORTAL.md rules off the customer's page, as a key
    /// substring: `PortalDoc`'s JSON must never carry one, at any depth.
    fn assert_no_forbidden_keys(v: &Value) {
        const FORBIDDEN: &[&str] = &["cost", "attempt", "branch", "verdict"];
        match v {
            Value::Object(map) => {
                for (k, val) in map {
                    let lower = k.to_lowercase();
                    for word in FORBIDDEN {
                        assert!(
                            !lower.contains(word),
                            "PortalDoc must not carry a {word:?}-shaped key, found {k:?}"
                        );
                    }
                    assert_no_forbidden_keys(val);
                }
            }
            Value::Array(items) => items.iter().for_each(assert_no_forbidden_keys),
            _ => {}
        }
    }

    /// A project with everything the operator's page would show — an
    /// expensive attempt, a branch, a passing verdict, a real deploy —
    /// must still hand the customer a document with none of it: only
    /// what docs/PORTAL.md, "What they see" actually lists.
    #[test]
    fn portal_doc_never_carries_a_forbidden_key_even_when_the_project_has_everything() {
        let (_dir, f) = fixture();
        f.store
            .create_project(&Project {
                name: "equitizr".into(),
                purpose: "quote requests turned into automations".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();

        let mut args = BTreeMap::new();
        args.insert("host".to_string(), "prod.example.com".to_string());
        f.store
            .add_deploy_target(&DeployTarget {
                project: "equitizr".into(),
                name: "prod".into(),
                repo: "/repo".into(),
                scope: None,
                method: "deploy-command".into(),
                args,
                check_cmd: "true".into(),
                on_landing: true,
                smoke_url: Some("https://prod.example.com/".into()),
            })
            .unwrap();
        let deploy_id = f
            .store
            .start_deploy("equitizr", "prod", "abc123", 100, None)
            .unwrap();
        f.store
            .finish_deploy(
                deploy_id,
                101,
                true,
                "ok",
                None,
                "",
                Some(true),
                Some(r#"{"screenshot":"screenshot.png"}"#),
                Some(true),
                Some("[]"),
            )
            .unwrap();

        let landed = insert(
            &f,
            Task {
                repo: "/repo".into(),
                task: "Make the quote text say 'usually same day'. Implementation: update src/pricing/quote.rs around line 42, then check tests/e2e/pricing.rs:88.".into(),
                base_branch: "main".into(),
                branch: "task-1-branch".into(),
                model: "sonnet".into(),
                max_turns: 10,
                max_attempts: 1,
                timeout_secs: 60,
                state: TaskState::Succeeded,
                created_at: crate::unix_now(),
                finished_at: Some(crate::unix_now()),
                landed_sha: "deadbeef".into(),
                workflow: "direct".into(),
                project: Some("equitizr".into()),
                ..Default::default()
            },
        );
        f.store
            .insert_attempt(&Attempt {
                task_id: landed.id,
                attempt_no: 1,
                step: "code".into(),
                state: AttemptState::Succeeded,
                started_at: 1,
                cost_usd: Some(12.5),
                verdict_json: r#"[{"name":"tests","ok":true}]"#.into(),
                ..Default::default()
            })
            .unwrap();

        let ini_id = f
            .store
            .create_initiative(&Initiative {
                project: "equitizr".into(),
                outcome: "quoting takes one click".into(),
                stop_after_same_rule: 3,
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        insert(
            &f,
            Task {
                repo: "/repo".into(),
                task: "which price sheet should this pull from?".into(),
                base_branch: "main".into(),
                branch: "task-2-branch".into(),
                model: "sonnet".into(),
                max_turns: 10,
                max_attempts: 1,
                timeout_secs: 60,
                state: TaskState::Blocked,
                reason: "needs input: which price sheet should this pull from?".into(),
                created_at: crate::unix_now(),
                workflow: "direct".into(),
                project: Some("equitizr".into()),
                initiative: Some(ini_id),
                ..Default::default()
            },
        );

        f.store
            .add_backlog("equitizr", "send a weekly summary")
            .unwrap();

        let p = f.store.project("equitizr").unwrap().unwrap();
        let doc = portal_doc(&f, &p).unwrap();
        assert_eq!(doc.deploy_targets.len(), 1);
        assert_eq!(doc.deploy_targets[0].where_it_runs, "prod.example.com");
        assert_eq!(doc.initiatives.len(), 1);
        assert_eq!(
            doc.initiatives[0].state, "waiting on you",
            "its only task is blocked on a question"
        );
        assert_eq!(doc.initiatives[0].pieces, 1);
        assert_eq!(doc.initiatives_more, 0);
        assert_eq!(doc.questions.len(), 1);
        assert_eq!(doc.landed.len(), 1);
        assert_eq!(
            doc.landed[0].text, "Make the quote text say 'usually same day'.",
            "first sentence only, no path-like tokens from the rest of the request"
        );
        assert_eq!(
            doc.landed[0].pieces, None,
            "a standalone task, not an initiative"
        );
        assert_eq!(doc.landed_more, 0);
        assert_eq!(doc.backlog.len(), 1);

        let v = serde_json::to_value(&doc).unwrap();
        assert_no_forbidden_keys(&v);
    }

    /// Every string value in `v`, scanned for the shapes the operator's
    /// own tools carry that a customer's page must never: a slash path or
    /// file extension, a dollar amount, or the words attempt, verdict,
    /// branch, commit or sha. `screenshot` is excluded: an internal file
    /// path the portal only ever opens server-side (see
    /// `portal::screenshot_path`), never renders as text.
    fn assert_no_forbidden_value_patterns(v: &Value) {
        const WORDS: &[&str] = &["attempt", "verdict", "branch", "commit", "sha"];
        match v {
            Value::Object(map) => {
                for (k, val) in map {
                    if k == "screenshot" {
                        continue;
                    }
                    assert_no_forbidden_value_patterns(val);
                }
            }
            Value::Array(items) => items.iter().for_each(assert_no_forbidden_value_patterns),
            Value::String(s) => {
                let lower = s.to_lowercase();
                for word in WORDS {
                    assert!(
                        !lower.contains(word),
                        "value {s:?} carries the forbidden word {word:?}"
                    );
                }
                assert!(!s.contains('$'), "value {s:?} looks like a dollar amount");
                for word in s.split_whitespace() {
                    assert!(
                        !crate::render::is_path_like_word(word),
                        "value {s:?} carries a path-like token {word:?}"
                    );
                }
            }
            _ => {}
        }
    }

    /// Equitizr's real record: a landed task whose text is thousands of
    /// words of engineering instructions — file paths, line numbers, a
    /// dollar budget, and the operator's own attempt/verdict/branch/
    /// commit/sha vocabulary — after its first sentence. `PortalDoc` must
    /// hand the customer only that first sentence, path-like tokens
    /// stripped; a second landed task, filed in the customer's own words
    /// (`title` set, as `forge add --title`/the concierge would), must
    /// hand back exactly that title and nothing of its own operator text.
    #[test]
    fn portal_doc_strips_operator_language_from_equitizrs_real_record() {
        let (_dir, f) = fixture();
        f.store
            .create_project(&Project {
                name: "equitizr".into(),
                purpose: "quote requests turned into automations".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();

        let engineering_instructions = format!(
            "Make the quote widget always show the annual discount. \
             Implementation: update src/pricing/discount.rs around line 154 \
             to add the annual multiplier, then check tests/e2e/pricing.rs:88 \
             for the assertion. {filler} Keep the attempt's cost under $2.50; \
             the verdict must show branch task/annual-discount landing clean \
             with commit sha abc1234def5678.",
            filler = "Typecheck, lint and tests must pass. ".repeat(50),
        );
        let untitled = insert(
            &f,
            Task {
                repo: "/repo".into(),
                task: engineering_instructions,
                base_branch: "main".into(),
                branch: "task-long-branch".into(),
                model: "sonnet".into(),
                max_turns: 10,
                max_attempts: 1,
                timeout_secs: 60,
                state: TaskState::Succeeded,
                created_at: crate::unix_now(),
                finished_at: Some(1_700_000_000),
                landed_sha: "deadbeef".into(),
                workflow: "direct".into(),
                project: Some("equitizr".into()),
                ..Default::default()
            },
        );
        assert!(untitled.title.is_none());

        insert(
            &f,
            Task {
                repo: "/repo".into(),
                title: Some("Show the annual discount on every quote".into()),
                task: "internal: wire src/pricing/discount.rs into the quote flow; \
                       verdict must pass, branch task/x, commit sha deadbeef, \
                       attempt cost $9.99"
                    .into(),
                base_branch: "main".into(),
                branch: "task-titled-branch".into(),
                model: "sonnet".into(),
                max_turns: 10,
                max_attempts: 1,
                timeout_secs: 60,
                state: TaskState::Succeeded,
                created_at: crate::unix_now(),
                finished_at: Some(1_700_000_100),
                landed_sha: "cafef00d".into(),
                workflow: "direct".into(),
                project: Some("equitizr".into()),
                ..Default::default()
            },
        );

        let p = f.store.project("equitizr").unwrap().unwrap();
        let doc = portal_doc(&f, &p).unwrap();
        assert_eq!(doc.landed.len(), 2);
        // Newest first: the titled task landed a hundred seconds later.
        assert_eq!(
            doc.landed[0].text,
            "Show the annual discount on every quote"
        );
        assert_eq!(
            doc.landed[1].text,
            "Make the quote widget always show the annual discount."
        );
        assert_eq!(doc.landed[0].pieces, None);
        assert_eq!(doc.landed[1].pieces, None);

        let v = serde_json::to_value(&doc).unwrap();
        assert_no_forbidden_keys(&v);
        assert_no_forbidden_value_patterns(&v);
    }

    /// Done merges landed initiatives and standalone landed tasks into
    /// one newest-first list, an initiative's line carrying how many
    /// tasks it took; past ten, the rest collapse into `landed_more`
    /// rather than growing the page. Being built treats open initiatives
    /// the same way (see docs/PORTAL.md).
    #[test]
    fn done_and_being_built_are_newest_first_and_cap_at_ten() {
        let (_dir, f) = fixture();
        f.store
            .create_project(&Project {
                name: "acme".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();

        // A landed initiative: two tasks, both succeeded, settled.
        let ini_id = f
            .store
            .create_initiative(&Initiative {
                project: "acme".into(),
                outcome: "checkout redesign shipped".into(),
                stop_after_same_rule: 3,
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        for n in 0..2 {
            insert(
                &f,
                Task {
                    repo: "/repo".into(),
                    task: format!("checkout piece {n}"),
                    base_branch: "main".into(),
                    branch: format!("ini-branch-{n}"),
                    model: "sonnet".into(),
                    max_turns: 10,
                    max_attempts: 1,
                    timeout_secs: 60,
                    state: TaskState::Succeeded,
                    created_at: crate::unix_now(),
                    finished_at: Some(crate::unix_now()),
                    landed_sha: format!("sha{n}"),
                    workflow: "direct".into(),
                    project: Some("acme".into()),
                    initiative: Some(ini_id),
                    ..Default::default()
                },
            );
        }
        f.store.settle_initiative(ini_id, 1_700_000_006).unwrap();

        // Twelve standalone landed tasks, oldest to newest, no initiative.
        for n in 0..12 {
            insert(
                &f,
                Task {
                    repo: "/repo".into(),
                    task: format!("Ship improvement number {n}."),
                    base_branch: "main".into(),
                    branch: format!("solo-branch-{n}"),
                    model: "sonnet".into(),
                    max_turns: 10,
                    max_attempts: 1,
                    timeout_secs: 60,
                    state: TaskState::Succeeded,
                    created_at: crate::unix_now(),
                    finished_at: Some(1_700_000_000 + n),
                    landed_sha: format!("solo{n}"),
                    workflow: "direct".into(),
                    project: Some("acme".into()),
                    ..Default::default()
                },
            );
        }

        // Twelve open initiatives, oldest to newest by created_at.
        for n in 0..12 {
            f.store
                .create_initiative(&Initiative {
                    project: "acme".into(),
                    outcome: format!("open initiative {n}"),
                    stop_after_same_rule: 3,
                    created_at: 1_600_000_000 + n,
                    ..Default::default()
                })
                .unwrap();
        }

        let p = f.store.project("acme").unwrap().unwrap();
        let doc = portal_doc(&f, &p).unwrap();

        // Done: 13 total landed items (1 initiative + 12 tasks), capped at
        // ten, three more.
        assert_eq!(doc.landed.len(), 10);
        assert_eq!(doc.landed_more, 3);
        assert_eq!(
            doc.landed[0].text, "Ship improvement number 11.",
            "newest standalone task first"
        );
        let checkout = doc
            .landed
            .iter()
            .find(|l| l.text == "checkout redesign shipped")
            .expect("the landed initiative's own line");
        assert_eq!(checkout.pieces, Some(2), "two pieces of work");

        // Being built: 12 open initiatives, capped at ten, two more.
        assert_eq!(doc.initiatives.len(), 10);
        assert_eq!(doc.initiatives_more, 2);
        assert_eq!(
            doc.initiatives[0].outcome, "open initiative 11",
            "newest open initiative first"
        );
        assert_eq!(doc.initiatives[0].pieces, 0, "no tasks filed on it yet");
    }

    /// "Running for you" continued: every run workflow the project's jobs
    /// have used, newest job first, each capped at its last three, a
    /// failure's reason cut to one line with any path-like token stripped
    /// (see docs/PORTAL.md).
    #[test]
    fn run_workflows_list_the_last_three_jobs_each_newest_first() {
        use crate::store::{Job, JobState};

        let (_dir, f) = fixture();
        f.store
            .create_project(&Project {
                name: "acme".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();

        // Four jobs for "nightly-sync": only the newest three should
        // survive on its entry.
        for n in 0..4 {
            f.store
                .create_job(&Job {
                    project: "acme".into(),
                    workflow: "nightly-sync".into(),
                    state: JobState::Ok,
                    started_at: 1_700_000_000 + n,
                    ..Default::default()
                })
                .unwrap();
        }
        // One failing job for "weekly-report", its reason cut from the
        // first failing check's tail, path-like tokens stripped.
        f.store
            .create_job(&Job {
                project: "acme".into(),
                workflow: "weekly-report".into(),
                state: JobState::Failed,
                started_at: 1_700_000_500,
                verdict_json: serde_json::to_string(&[crate::checks::CheckResult {
                    level: "OP".into(),
                    name: "send-report".into(),
                    ok: false,
                    tail: "could not reach src/report/send.rs:12, the mailer timed out".into(),
                    ..Default::default()
                }])
                .unwrap(),
                ..Default::default()
            })
            .unwrap();
        // A needs-human job for "weekly-report" too, more recent than the
        // failure above.
        f.store
            .create_job(&Job {
                project: "acme".into(),
                workflow: "weekly-report".into(),
                state: JobState::NeedsHuman,
                started_at: 1_700_000_600,
                verdict_json: serde_json::to_string(&[crate::checks::CheckResult {
                    level: "L0".into(),
                    name: "budget".into(),
                    ok: false,
                    tail: "over the per-run budget".into(),
                    ..Default::default()
                }])
                .unwrap(),
                ..Default::default()
            })
            .unwrap();

        let p = f.store.project("acme").unwrap().unwrap();
        let doc = portal_doc(&f, &p).unwrap();

        assert_eq!(doc.run_workflows.len(), 2);
        // Most recent job first, so "weekly-report" (started at 600)
        // sorts ahead of "nightly-sync" (started at 103 at the newest).
        assert_eq!(doc.run_workflows[0].name, "weekly-report");
        assert_eq!(doc.run_workflows[0].jobs.len(), 2);
        assert_eq!(doc.run_workflows[0].jobs[0].state, "needs_human");
        assert_eq!(
            doc.run_workflows[0].jobs[0].reason.as_deref(),
            Some("over the per-run budget")
        );
        assert_eq!(doc.run_workflows[0].jobs[1].state, "failed");
        assert_eq!(
            doc.run_workflows[0].jobs[1].reason.as_deref(),
            Some("could not reach the mailer timed out"),
            "path-like token stripped from the tail"
        );

        assert_eq!(doc.run_workflows[1].name, "nightly-sync");
        assert_eq!(doc.run_workflows[1].jobs.len(), 3, "capped at three");
        assert_eq!(doc.run_workflows[1].jobs[0].started_at, 1_700_000_003);
        assert_eq!(doc.run_workflows[1].jobs[0].state, "ok");
        assert_eq!(doc.run_workflows[1].jobs[0].reason, None);

        let v = serde_json::to_value(&doc).unwrap();
        assert_no_forbidden_keys(&v);
    }
}

#[cfg(test)]
mod time_to_live_tests {
    use super::*;
    use crate::ctx::Paths;
    use crate::store::{Store, TaskState};

    fn fixture() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        (dir, f)
    }

    /// A landed task on `workflow` that took `secs` from creation to landing.
    fn landed(f: &Forge, workflow: &str, hash: &str, secs: i64) -> Task {
        let mut t = Task {
            repo: "/repo".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: 1_000,
            started_at: Some(1_000),
            finished_at: Some(1_000 + secs),
            landed_sha: format!("sha-{workflow}"),
            landed_at: Some(1_000 + secs),
            workflow: workflow.into(),
            workflow_hash: hash.into(),
            ..Default::default()
        };
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();
        // The git-derived caches `stats_doc` refreshes, filled so the
        // refresh has nothing to compute against the fixture's fake repo.
        f.store.set_churn_cache(t.id, 0, 0, i64::MAX / 2).unwrap();
        f.store
            .set_repair_cost_cache(t.id, 0.0, i64::MAX / 2)
            .unwrap();
        f.store.set_hand_commits_cache(t.id, 0, 1).unwrap();
        t
    }

    #[tokio::test]
    async fn stats_doc_carries_one_time_to_live_row_per_workflow() {
        let (_dir, f) = fixture();
        landed(&f, "direct", "h-direct", 100);
        landed(&f, "tdd", "h-tdd", 700);

        let doc = stats_doc(&f, &crate::store::StatsFilter::default(), None)
            .await
            .unwrap();
        assert_eq!(doc.time_to_live.len(), 2, "one row per workflow");
        let row = |wf: &str| doc.time_to_live.iter().find(|r| r.workflow == wf).unwrap();
        let (d, t) = (row("direct"), row("tdd"));
        assert_eq!((d.hash.as_str(), d.n), ("h-direct", 1));
        assert_eq!((d.median_secs, d.p90_secs), (Some(100.0), Some(100.0)));
        assert_eq!((t.hash.as_str(), t.n), ("h-tdd", 1));
        assert_eq!((t.median_secs, t.p90_secs), (Some(700.0), Some(700.0)));

        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(v["time_to_live"].as_array().unwrap().len(), 2, "{v}");
    }
}
