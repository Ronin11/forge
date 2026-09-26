use crate::ctx::Forge;
use crate::store::{Decision, Message, RoleRouting, Task, TaskRef, TaskState, TaskSummary};
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
    /// Trust the caller earned by the path it queued through: `"operator"`,
    /// `"contact"`, or `"public"` (see `store::Trust`).
    pub trust: String,
    /// Only under `forge log --touches`: `"changes"` when an attempt
    /// recorded a change at the path, `"text"` when only the task's text
    /// mentions it (`--touches-text`). Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub touch: Option<String>,
    /// Only under `forge log --grep`: which field matched, the first of
    /// `"text"` (the task text, or an exact id), `"title"`, `"plan"`,
    /// `"summary"` (the last attempt with an envelope). Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<String>,
    /// Only under `forge log --failed-on` or `--reason`: the attempts
    /// that matched, each `{attempt_no, step, reason, name, tail}` with
    /// `name` the failing verdict row (null for a reason match) and
    /// `tail` that row's first line of output. Absent otherwise.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<crate::store::FailedAttempt>,
}

/// The rows behind `forge log` and the snapshot: the filter held to what
/// the record and the rule registry know (`--failed-on` names a rule or
/// a check the record has seen; anything else is refused naming both
/// lists), then the store's listing as `TaskRow`s.
pub fn task_rows(f: &Forge, q: &crate::store::TaskFilter) -> Result<Vec<TaskRow>> {
    if !q.failed_on.is_empty() {
        let seen = f.store.verdict_row_names()?;
        for name in &q.failed_on {
            if crate::verify::Rule::parse(name).is_none() && !seen.iter().any(|s| s == name) {
                let rules: Vec<&str> = crate::verify::Rule::ALL.iter().map(|r| r.name()).collect();
                anyhow::bail!(
                    "--failed-on {name:?}: no such rule or check. Rules: {}. Checks the record has seen: {}",
                    rules.join(", "),
                    if seen.is_empty() {
                        "none yet".to_string()
                    } else {
                        seen.join(", ")
                    }
                );
            }
        }
    }
    Ok(f.store
        .list_tasks_where(q)?
        .iter()
        .map(TaskRow::from)
        .collect())
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
            trust: s.trust.clone(),
            touch: s.touch.clone(),
            matched: s.matched.clone(),
            failures: s.failures.clone(),
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
    /// For a question with a `to`: when the newest outbound message on
    /// this task to that contact was recorded; `null` when none was.
    pub delivered_at: Option<i64>,
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
            delivered_at: None,
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
    /// `demotion-as-task` for the kernel's own ruling; absent otherwise.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub kind: String,
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
            kind: d.kind.clone(),
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
    /// Trust the caller earned by the path it queued through: `"operator"`,
    /// `"contact"`, or `"public"` (see `store::Trust`).
    pub trust: String,
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
    /// Who the task's question is addressed to; absent means the operator.
    pub to: Option<String>,
    /// For a question with a `to`: the newest outbound message row on
    /// this task to that contact, or `null` when none was recorded.
    pub delivered_at: Option<i64>,
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
    /// The claude CLI's own figure when `cost_usd` was priced at list.
    pub cli_cost_usd: Option<f64>,
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
        trust: t.trust.as_str().to_string(),
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
        to: t.question_to.clone(),
        delivered_at: match t.question_to.as_deref() {
            Some(to) => f.store.delivered_at(t.id, to)?,
            None => None,
        },
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
            cli_cost_usd: a.cli_cost_usd,
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
