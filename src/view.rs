//! The one place the CLI's machine-readable rows are shaped. `forge log`,
//! `forge requests` and `forge decisions` each have a text form and a
//! `--json` form; both render from the same struct here so the two forms
//! cannot drift apart.

use crate::store::{Decision, Task, TaskState, TaskSummary};
use serde::Serialize;

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
