//! The one place the CLI's machine-readable rows are shaped. `forge log`,
//! `forge requests` and `forge decisions` each have a text form and a
//! `--json` form; both render from the same struct here so the two forms
//! cannot drift apart.

use crate::ctx::Forge;
use crate::store::{
    Decision, JournalStat, StepStat, Task, TaskRef, TaskState, TaskSummary, WorkflowStat,
};
use crate::workflows::Problem;
use crate::{config, plugins};
use anyhow::Result;
use serde::Serialize;
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
/// landed, failed, or otherwise settled).
#[derive(Serialize)]
pub struct DecisionRow {
    pub id: i64,
    pub task_id: i64,
    pub repo: String,
    pub question: String,
    pub answer: String,
    pub created_at: i64,
    pub answered_by: String,
    pub citations: String,
    pub retry_id: Option<i64>,
    pub outcome: Option<String>,
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
    #[serde(skip)]
    pub worktree_removed_at: Option<i64>,
    #[serde(skip)]
    pub decisions: Vec<Decision>,
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
}

pub fn trace_doc(f: &Forge, t: &Task) -> Result<TraceDoc> {
    let attempts = f.store.attempts(t.id)?;
    let ops = f.store.ops(t.id)?;
    let diagnosis = crate::audit::diagnose(t, &attempts);
    let lineage = f.store.lineage(t.id)?;

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
    #[serde(flatten)]
    pub legacy: serde_json::Map<String, Value>,
}

impl From<&WorkflowStat> for StatsWorkflowRow {
    fn from(w: &WorkflowStat) -> Self {
        let cost_per_success_usd = (w.succeeded > 0).then(|| w.cost / w.succeeded as f64);
        let cost_per_landed_usd = (w.landed > 0).then(|| w.cost / w.landed as f64);
        let broke_base_share = (w.landed > 0).then(|| w.broke_base as f64 / w.landed as f64);
        let repaired_share = (w.landed > 0).then(|| w.repaired as f64 / w.landed as f64);
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

/// One row of `StatsDoc.by_role`: attempts, outcomes, cost and wall time
/// for one (role, provider, model) combination, role being the attempt's
/// step, as `forge stats --by-role` shows it.
#[derive(Serialize)]
pub struct StatsRoleRow {
    pub role: String,
    pub provider: String,
    pub model: String,
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
}

impl From<&crate::store::RoleStat> for StatsRoleRow {
    fn from(r: &crate::store::RoleStat) -> Self {
        let broke_base_share = match (r.landed, r.broke_base) {
            (Some(landed), Some(broke)) if landed > 0 => Some(broke as f64 / landed as f64),
            _ => None,
        };
        StatsRoleRow {
            role: r.role.clone(),
            provider: r.provider.clone(),
            model: r.model.clone(),
            attempts: r.attempts,
            succeeded: r.succeeded,
            succeeded_share: (r.attempts > 0).then(|| r.succeeded as f64 / r.attempts as f64),
            mean_turns: r.mean_turns,
            mean_cost_usd: r.mean_cost_usd,
            mean_secs: r.mean_ms / 1000.0,
            landed: r.landed,
            broke_base: r.broke_base,
            broke_base_share,
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
    /// Attempts, outcomes, cost and wall time per (role, provider, model);
    /// see `forge stats --by-role`.
    pub by_role: Vec<StatsRoleRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
}

pub fn stats_doc(f: &Forge, scope: &crate::store::StatsFilter) -> Result<StatsDoc> {
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
    let projects = if scope.project.is_none() && scope.initiative.is_none() {
        f.store.project_stats()?.iter().map(Into::into).collect()
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
        by_role: f.store.role_stats()?.iter().map(Into::into).collect(),
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
    pub workflow: Option<String>,
    pub per_task_usd: Option<f64>,
    pub per_initiative_usd: Option<f64>,
    pub supervisor_model: Option<String>,
    pub supervisor_per_lineage: Option<i64>,
    pub protected: Vec<String>,
    pub role_providers: std::collections::BTreeMap<String, String>,
}

pub fn project_row(f: &Forge, p: &crate::store::Project) -> Result<ProjectRow> {
    let repos = f
        .store
        .project_repos(&p.name)?
        .iter()
        .map(ProjectRepoRow::from)
        .collect();
    let cost = f.store.project_task_stats(&p.name)?.cost;
    let tasks = f.store.project_tasks(&p.name)?;
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
        purpose: p.purpose.clone(),
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
        workflow: p.workflow.clone(),
        per_task_usd: p.per_task_usd,
        per_initiative_usd: p.per_initiative_usd,
        supervisor_model: p.supervisor_model.clone(),
        supervisor_per_lineage: p.supervisor_per_lineage,
        protected: p.protected.clone().unwrap_or_default(),
        role_providers: p.role_providers.clone(),
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
fn l0_rule_of(reason: &str) -> Option<String> {
    let rest = reason.strip_prefix("L0 failed: ")?;
    let rest = rest.split(" (after").next().unwrap_or(rest).trim();
    (!rest.is_empty()).then(|| rest.to_string())
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

/// `Some` when the worker is currently holding new claims for this
/// initiative (see docs/PROJECTS.md, "Stop rule and budget"): its summed
/// cost has reached its budget (reported as `"budget"`), or its trailing
/// run of failed tasks all blame the same L0 rule and that run has
/// reached `stop_after_same_rule` (reported as that rule's name).
pub fn initiative_hold(f: &Forge, ini: &crate::store::Initiative) -> Result<Option<String>> {
    let budget = ini.budget_usd.or_else(|| {
        f.store
            .project(&ini.project)
            .ok()
            .flatten()
            .and_then(|p| p.per_initiative_usd)
    });
    if let Some(b) = budget
        && f.store.initiative_cost(ini.id)? >= b
    {
        return Ok(Some("budget".to_string()));
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
    let mut rule: Option<String> = None;
    let mut len = 0i64;
    for t in terminal {
        match (t.state, l0_rule_of(&t.reason)) {
            (TaskState::Failed, Some(r)) => {
                if rule.as_deref() == Some(r.as_str()) {
                    len += 1;
                } else {
                    len = 1;
                    rule = Some(r);
                }
            }
            _ => {
                len = 0;
                rule = None;
            }
        }
    }
    Ok((len >= ini.stop_after_same_rule).then_some(rule).flatten())
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
    pub cost_usd: f64,
    pub elapsed_secs: Option<i64>,
    pub created_at: i64,
    pub settled_at: Option<i64>,
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
    let rulings = decisions
        .iter()
        .filter(|d| d.answered_by == "supervisor")
        .map(|d| InitiativeRulingRow {
            task_id: d.task_id,
            question: d.question.clone(),
            answer: d.answer.clone(),
            citations: d.citations.clone(),
        })
        .collect();
    let mut questions: Vec<InitiativeQuestionRow> = decisions
        .iter()
        .filter(|d| d.answered_by == "operator")
        .map(|d| InitiativeQuestionRow {
            task_id: d.task_id,
            question: d.question.clone(),
            answer: Some(d.answer.clone()),
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
    let refused = refused_counts(f, &tasks)?;
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
            .map(|(t, retries)| InitiativeTaskRow {
                id: t.id,
                state: t.state.as_str().to_string(),
                reason: t.reason.clone(),
                retries: *retries,
            })
            .collect(),
        refused,
        rulings,
        questions,
        cost_usd: cost,
        elapsed_secs: elapsed,
        created_at: ini.created_at,
        settled_at: ini.settled_at,
    })
}

#[cfg(test)]
mod stats_tests {
    use super::*;
    use crate::store::{StepStat, WorkflowStat};

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
        };
        let row = StatsWorkflowRow::from(&w);
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["broke_base"], 1);
        assert_eq!(v["broke_base_share"], 0.25);
        assert_eq!(v["repaired"], 2);
        assert_eq!(v["repaired_share"], 0.5);
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
            by_role: vec![],
            tools: None,
        };
        let v = serde_json::to_value(&doc).unwrap();
        assert!(v.get("tools").is_none(), "{v}");
        assert!(v.get("projects").is_none(), "{v}");
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
