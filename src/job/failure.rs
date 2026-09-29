//! What happens to a job that ended `Failed` or `NeedsHuman` (docs/JOBS.md
//! step 5, "The human rung"): the `[limits] on_failure` policy, who to ask
//! and what to tell them, and the retry that requeues the same input.

use super::input;
use crate::ctx::Forge;
use crate::store::{Job, JobEffect, JobState, Task, TaskState};
use crate::workflows;
use crate::{checks, unix_now};
use anyhow::Result;

/// docs/JOBS.md step 5 ("The human rung"): what `[limits] on_failure` says
/// to do with a job that just ended `Failed` or `NeedsHuman` — pure, no
/// I/O, so the policy itself is unit-testable apart from the store and
/// filesystem writes `run_now` does with its answer.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FailureAction {
    /// Requeue with the same input; the new job's `retry_count` is one
    /// more than this one's.
    Retry,
    /// `drop`, or a `retry:N` whose budget is already spent: the job's own
    /// recorded state is the last word.
    Stop,
    /// File a question addressed to this contact (`None`: the operator).
    Ask(Option<String>),
}

pub(super) fn decide_on_failure(
    on_failure: &workflows::OnFailure,
    retry_count: i64,
    trigger_contact: Option<&str>,
) -> FailureAction {
    match on_failure {
        workflows::OnFailure::Drop => FailureAction::Stop,
        workflows::OnFailure::Retry(n) => {
            if retry_count < i64::from(*n) {
                FailureAction::Retry
            } else {
                FailureAction::Stop
            }
        }
        workflows::OnFailure::AskOperator => FailureAction::Ask(None),
        workflows::OnFailure::AskContact => FailureAction::Ask(trigger_contact.map(str::to_string)),
    }
}

/// The contact `ask:contact` addresses (docs/JOBS.md, "The human rung"):
/// the sender who actually fired this job when its trigger was a message
/// (`from` in the job's input — `trigger_ref` is the message's id, the
/// mark that keeps one message from starting one workflow's job twice),
/// else the workflow's own `[trigger] contact` when that names someone
/// (not the `"*"` wildcard), else `None` — asked of the operator instead.
pub(crate) fn trigger_contact(
    job: &Job,
    trigger: Option<&workflows::Trigger>,
    input: &serde_json::Value,
) -> Option<String> {
    if job.trigger_kind == workflows::TriggerOn::Message.as_str()
        && let Some(from) = input
            .get("from")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    {
        return Some(from.to_string());
    }
    trigger.and_then(|t| t.contact.clone()).filter(|c| c != "*")
}

/// The question `ask:operator`/`ask:contact` files (docs/JOBS.md, "The
/// human rung"): the job id, its workflow, the assertion or step that
/// failed (with the tail of its output), and every effect the run logged — so whoever answers can see
/// what almost happened without re-running anything.
pub(super) fn failure_reason(
    job_id: i64,
    workflow: &str,
    verdict: &[checks::CheckResult],
    effects: &[JobEffect],
) -> String {
    let failed = verdict
        .iter()
        .find(|c| !c.ok)
        .map(|c| {
            if c.tail.trim().is_empty() {
                let exit = c
                    .exit
                    .map_or("no exit status".to_string(), |x| format!("exit {x}"));
                format!("{}: {exit}, no output", c.name)
            } else {
                format!("{}: {}", c.name, c.tail)
            }
        })
        .unwrap_or_else(|| "no check recorded which one failed".to_string());
    let effects = if effects.is_empty() {
        "none".to_string()
    } else {
        effects
            .iter()
            .map(|e| format!("- {} {}: {}", e.kind, e.target, e.summary))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!("job {job_id} ({workflow}) failed: {failed}\n\nEffects:\n{effects}")
}

/// The job a `job question`'s reason names: `failure_reason` opens with
/// `job <id> (`. `None` when the text is not one of ours.
pub(crate) fn question_job_id(reason: &str) -> Option<i64> {
    reason
        .strip_prefix("job ")?
        .split_once(" (")?
        .0
        .parse()
        .ok()
}

/// File a blocked no-work task on the project, the human rung a job's
/// `ask:*` on_failure ends at (docs/JOBS.md, "The human rung") — the same
/// shape `deploy::ask` files for a failed deploy, so `forge requests`, the
/// portal and the Signal plugin surface it the same way. Always a new
/// task: unlike a deploy target, a job has no single running task of its
/// own to reuse.
pub(super) fn ask(
    f: &Forge,
    project: &str,
    repo: &str,
    question_to: Option<&str>,
    reason: String,
) -> Result<()> {
    let mut t = Task {
        repo: repo.to_string(),
        task: "job question".to_string(),
        base_branch: String::new(),
        state: TaskState::Blocked,
        reason,
        question_to: question_to.map(str::to_string),
        created_at: unix_now(),
        workflow: "direct".to_string(),
        project: Some(project.to_string()),
        land: false,
        ..Default::default()
    };
    t.id = f.store.insert_task(&t)?;
    f.store.update_task(&t)?;
    Ok(())
}

/// Requeue a failed or needs-human job with the same input: `retry:N`'s
/// share of docs/JOBS.md's "The human rung". `trigger_kind`/`trigger_ref`
/// carry over unchanged — a retry of a schedule-triggered job is still
/// that slot's job, not a new firing — and `retry_count` is one more than
/// the job it retries, so the next failure's `decide_on_failure` can tell
/// when the budget is spent.
pub(super) async fn retry_job(f: &Forge, job: &Job, input_text: &str) -> Result<i64> {
    let retry = Job {
        id: 0,
        project: job.project.clone(),
        workflow: job.workflow.clone(),
        workflow_hash: job.workflow_hash.clone(),
        landed_sha: job.landed_sha.clone(),
        trigger_kind: job.trigger_kind.clone(),
        trigger_ref: job.trigger_ref.clone(),
        state: JobState::Scheduled,
        workflow_source: job.workflow_source.clone(),
        dry_run: false,
        started_at: unix_now(),
        finished_at: None,
        cost_usd: None,
        verdict_json: "[]".to_string(),
        due_at: None,
        retry_count: job.retry_count + 1,
    };
    let retry_id = f.store.create_job(&retry)?;
    input::publish(f, retry_id, input_text, None)?;
    Ok(retry_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_job(trigger_kind: &str, trigger_ref: &str, retry_count: i64) -> Job {
        Job {
            trigger_kind: trigger_kind.to_string(),
            trigger_ref: trigger_ref.to_string(),
            retry_count,
            ..Default::default()
        }
    }

    fn test_trigger(contact: Option<&str>) -> workflows::Trigger {
        workflows::Trigger {
            on: workflows::TriggerOn::Message,
            cron: None,
            contact: contact.map(str::to_string),
            name: None,
            r#type: None,
            delay: None,
        }
    }

    #[test]
    fn decide_on_failure_drops_and_never_retries_or_asks() {
        assert_eq!(
            decide_on_failure(&workflows::OnFailure::Drop, 0, None),
            FailureAction::Stop
        );
        assert_eq!(
            decide_on_failure(&workflows::OnFailure::Drop, 5, Some("mary")),
            FailureAction::Stop
        );
    }

    #[test]
    fn decide_on_failure_retries_until_its_budget_is_spent_then_stops() {
        let policy = workflows::OnFailure::Retry(1);
        assert_eq!(decide_on_failure(&policy, 0, None), FailureAction::Retry);
        assert_eq!(decide_on_failure(&policy, 1, None), FailureAction::Stop);
        assert_eq!(decide_on_failure(&policy, 2, None), FailureAction::Stop);
    }

    #[test]
    fn decide_on_failure_asks_the_operator_regardless_of_any_contact() {
        assert_eq!(
            decide_on_failure(&workflows::OnFailure::AskOperator, 0, Some("mary")),
            FailureAction::Ask(None)
        );
        assert_eq!(
            decide_on_failure(&workflows::OnFailure::AskOperator, 3, None),
            FailureAction::Ask(None)
        );
    }

    #[test]
    fn decide_on_failure_asks_the_contact_when_there_is_one_else_the_operator() {
        assert_eq!(
            decide_on_failure(&workflows::OnFailure::AskContact, 0, Some("mary")),
            FailureAction::Ask(Some("mary".to_string()))
        );
        assert_eq!(
            decide_on_failure(&workflows::OnFailure::AskContact, 0, None),
            FailureAction::Ask(None)
        );
    }

    #[test]
    fn trigger_contact_prefers_the_message_triggers_own_sender() {
        let job = test_job("message", "42", 0);
        let trigger = test_trigger(Some("customers"));
        let input = serde_json::json!({"from": "+15555550100", "message_id": 42});
        assert_eq!(
            trigger_contact(&job, Some(&trigger), &input).as_deref(),
            Some("+15555550100"),
            "a specific sender beats the workflow's own contact group"
        );
    }

    #[test]
    fn trigger_contact_falls_back_to_the_workflows_own_contact_field() {
        let job = test_job("manual", "", 0);
        let trigger = test_trigger(Some("customers"));
        assert_eq!(
            trigger_contact(&job, Some(&trigger), &serde_json::json!({})).as_deref(),
            Some("customers")
        );
    }

    #[test]
    fn trigger_contact_ignores_a_message_job_with_no_sender_in_its_input() {
        let job = test_job("message", "42", 0);
        let trigger = test_trigger(Some("customers"));
        assert_eq!(
            trigger_contact(&job, Some(&trigger), &serde_json::json!({})).as_deref(),
            Some("customers")
        );
        let wildcard = test_trigger(Some("*"));
        assert_eq!(
            trigger_contact(&job, Some(&wildcard), &serde_json::json!({})),
            None,
            "the wildcard names no one to ask"
        );
    }

    #[test]
    fn trigger_contact_is_none_with_no_trigger_and_no_sender() {
        let job = test_job("manual", "", 0);
        assert_eq!(trigger_contact(&job, None, &serde_json::json!({})), None);
    }

    #[test]
    fn failure_reason_names_the_job_the_failed_check_and_every_effect() {
        let verdict = vec![checks::CheckResult {
            level: "L0".into(),
            name: "clean".into(),
            ok: false,
            tail: "effect log is not empty".into(),
            ..Default::default()
        }];
        let effects = vec![JobEffect {
            job_id: 12,
            seq: 0,
            kind: "row".into(),
            target: "sonnet".into(),
            summary: "sonnet resolved to claude-sonnet-4-5 last week, claude-sonnet-5 this week"
                .into(),
            ..Default::default()
        }];
        let reason = failure_reason(12, "drift-weekly", &verdict, &effects);
        assert!(reason.contains("job 12"), "{reason}");
        assert!(reason.contains("drift-weekly"), "{reason}");
        assert!(
            reason.contains("clean: effect log is not empty"),
            "{reason}"
        );
        assert!(
            reason.contains("sonnet resolved to claude-sonnet-4-5"),
            "{reason}"
        );
    }

    #[test]
    fn failure_reason_says_so_when_nothing_failed_or_was_logged() {
        let reason = failure_reason(3, "wf", &[], &[]);
        assert!(
            reason.contains("no check recorded which one failed"),
            "{reason}"
        );
        assert!(reason.contains("Effects:\nnone"), "{reason}");
    }
}
