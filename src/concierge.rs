//! `forge ask`: the front door, not the interview (see docs/INTAKE.md,
//! "The front door is not the interview"). A customer's message is a
//! `request`, a `question`, a `need`, or `unclear`; the `concierge`
//! directive, given the project's purpose, brief, backlog, deploy
//! targets and last twenty tasks, sorts it into one of those and this
//! module acts on the decision: files the task, prints the answer, files
//! an intake task, or blocks a small placeholder task with the question
//! addressed to the contact. Every decision is recorded: a
//! `concierge_json` column on the task it produced, or a `decisions` row
//! for an answer.

use crate::ctx::Forge;
use crate::queue::{self, TaskRequest};
use crate::store::TaskState;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

/// What the `concierge` directive returns in its `summary`: which of the
/// four kinds, and the one field that kind needs. Everything else is
/// left empty and ignored.
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
struct Decision {
    kind: String,
    /// `request`: the task text to file.
    task: String,
    /// `question`: the answer, in plain words.
    answer: String,
    /// `need`: one sentence saying why an interview is warranted.
    reason: String,
    /// `unclear`: the one question to ask.
    question: String,
}

/// What `forge ask` did with the message, for the CLI to report.
pub enum Asked {
    Filed { task: i64 },
    Answered { answer: String, decision: i64 },
    Need { task: i64, reason: String },
    Unclear { task: i64, question: String },
}

/// A `TaskRequest` with the CLI's own defaults (`TaskArgs`'s
/// `default_value_t`s), the only fields every branch below needs to fill
/// in: the repo, the text, the project, and which workflow runs it.
fn base(project: &str, repo: &str, task: String, workflow: &str) -> TaskRequest {
    TaskRequest {
        repo: PathBuf::from(repo),
        task,
        project: Some(project.to_string()),
        workflow: Some(workflow.to_string()),
        max_turns: 100,
        retries: 1,
        timeout_secs: 1800,
        ..Default::default()
    }
}

/// Run the concierge on `message` and act on its decision. `f` is an
/// `Arc` because reaching the decision means running the `concierge`
/// workflow to completion, the same way `forge run` drives a task.
pub async fn ask(f: Arc<Forge>, project: &str, message: &str, from: Option<&str>) -> Result<Asked> {
    let repo = f
        .store
        .first_repo(project)?
        .with_context(|| format!("project {project} lists no repository"))?;

    let mut req = base(project, &repo, message.to_string(), "concierge");
    req.retries = 0;
    req.no_land = true;
    let t = queue::enqueue(&f, &req, None).await?;
    if !f.store.claim(t.id, std::process::id() as i64)? {
        bail!(
            "task {} was claimed by another worker before the concierge could run it",
            t.id
        );
    }
    let state = crate::worker::drive(f.clone(), t.id).await?;
    let t = f
        .store
        .task(t.id)?
        .with_context(|| format!("no task {}", t.id))?;
    if state != TaskState::Succeeded {
        bail!(
            "the concierge did not reach a decision (task {} is {}): {}",
            t.id,
            state.as_str(),
            t.reason
        );
    }
    if t.plan.is_empty() {
        bail!("the concierge task {} recorded no decision", t.id);
    }
    let raw = t.plan.clone();
    let d: Decision = serde_json::from_str(&raw).with_context(|| {
        format!(
            "task {}'s decision does not fit the concierge's schema: {raw}",
            t.id
        )
    })?;

    match d.kind.as_str() {
        "request" => {
            if d.task.trim().is_empty() {
                bail!("the concierge called this a request but named no task text");
            }
            // An ordinary task: it lands like any other once verified.
            let req = base(project, &repo, d.task.clone(), "direct");
            let mut n = queue::enqueue(&f, &req, None).await?;
            n.concierge_json = Some(raw);
            f.store.update_task(&n)?;
            Ok(Asked::Filed { task: n.id })
        }
        "question" => {
            if d.answer.trim().is_empty() {
                bail!("the concierge called this a question but gave no answer");
            }
            let decision = f.store.insert_decision_by(
                t.id, &repo, message, &d.answer, "concierge", "", from,
            )?;
            Ok(Asked::Answered {
                answer: d.answer,
                decision,
            })
        }
        "need" => {
            // "Contact: X." is the same convention an intake task's text
            // already carries when a person is named (see
            // tests/e2e/intake.rs); the interview reads the contact back
            // out of the task text the same way.
            let text = match from {
                Some(c) => format!("{message} Contact: {c}."),
                None => message.to_string(),
            };
            let mut req = base(project, &repo, text, "intake");
            req.retries = 0;
            req.no_land = true;
            let mut n = queue::enqueue(&f, &req, None).await?;
            n.concierge_json = Some(raw);
            f.store.update_task(&n)?;
            Ok(Asked::Need {
                task: n.id,
                reason: d.reason,
            })
        }
        "unclear" => {
            if d.question.trim().is_empty() {
                bail!("the concierge called this unclear but asked no question");
            }
            let mut req = base(project, &repo, message.to_string(), "direct");
            req.retries = 0;
            req.no_land = true;
            let mut n = queue::enqueue(&f, &req, None).await?;
            n.state = TaskState::Blocked;
            n.reason = format!("needs input: {}", d.question);
            n.question_to = from.map(str::to_string);
            n.concierge_json = Some(raw);
            f.store.update_task(&n)?;
            Ok(Asked::Unclear {
                task: n.id,
                question: d.question,
            })
        }
        other => bail!("the concierge returned an unknown decision kind {other:?}"),
    }
}
