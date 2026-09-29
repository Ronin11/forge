//! Answering a task blocked on a question: the answer is held to its
//! caller's scope, recorded as a decision, and re-queues the task.

use super::*;

/// Refuses an answer that reaches outside its caller's own project and
/// recipient. `scope` is `None` for the operator's own answers, which
/// stay unchecked; `Some((project, recipient))` — set only when a
/// caller like the portal names both — refuses a task whose `project`
/// is not that project, or whose `question_to` names anyone but that
/// recipient (a bare question, addressed to the operator, is never a
/// named recipient's to answer), naming both the task's own value and
/// what the caller expected.
fn check_answer_scope(
    id: i64,
    project: Option<&str>,
    question_to: Option<&str>,
    scope: Option<(&str, &str)>,
) -> Result<()> {
    let Some((expected_project, expected_recipient)) = scope else {
        return Ok(());
    };
    if project != Some(expected_project) {
        bail!(
            "task {id} belongs to project {}, not {expected_project}",
            project.unwrap_or("(none)")
        );
    }
    if question_to != Some(expected_recipient) {
        bail!(
            "task {id}'s question is addressed to {}, not {expected_recipient}",
            question_to.unwrap_or("the operator")
        );
    }
    Ok(())
}

/// Answer a task blocked on a question: record the decision, re-queue
/// the task as a retry whose text carries the answer, and point the
/// decision at it. `by` is "operator" or "supervisor"; `citations` is
/// what a supervisor's answer rests on. `scope` restricts the answer to
/// one project and recipient (see `check_answer_scope`); pass `None` for
/// the operator's own answers. Returns the decision and the new task. A
/// job or deploy question has no attempt to retry: its answer settles
/// the question instead (`job_question`), returning that task itself.
pub async fn answer(
    f: &Forge,
    id: i64,
    text: &str,
    by: &str,
    citations: &str,
    scope: Option<(&str, &str)>,
) -> Result<(i64, Task)> {
    let Some(old) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    check_answer_scope(
        id,
        old.project.as_deref(),
        old.question_to.as_deref(),
        scope,
    )?;
    if job_question::is_no_work_question(f, &old)? {
        return job_question::answer(f, &old, text, by, citations);
    }
    let last = f
        .store
        .attempts(id)?
        .into_iter()
        .rev()
        .find(|a| a.is_agent());
    if old.state != TaskState::Blocked
        || !matches!(
            last.as_ref().map(|a| a.state),
            Some(crate::store::AttemptState::NeedsInput)
        )
    {
        bail!(
            "task {id} is not blocked on a question (state {}); only that is answered",
            old.state.as_str()
        );
    }
    let question = last
        .and_then(|a| serde_json::from_str::<crate::envelope::Envelope>(&a.envelope_json).ok())
        .and_then(|e| e.needs_input)
        .map(|q| q.question)
        .with_context(|| format!("task {id}'s last attempt recorded no question"))?;
    let decision = f.store.insert_decision_by(crate::store::InsertDecisionBy {
        task_id: id,
        repo: &old.repo,
        question: &question,
        answer: text,
        answered_by: by,
        citations,
        answered_for: old.question_to.as_deref(),
    })?;
    let new_text = if by == "operator" {
        format!(
            "{}\n\nOperator's answer to a question from an earlier attempt: {text}",
            old.task
        )
    } else if by == "supervisor" {
        format!(
            "{}\n\nSupervisor's answer to a question from an earlier attempt (citing {citations}): {text}",
            old.task
        )
    } else {
        format!(
            "{}\n\n{by}'s answer to a question from an earlier attempt: {text}",
            old.task
        )
    };
    let after = old
        .after
        .iter()
        .map(|&d| map_dep(f, d, &std::collections::HashMap::new()))
        .collect::<Result<Vec<_>>>()?;
    let req = retry_request(&old, &RetryOverrides::none(), true, after, Some(new_text));
    let mut n = enqueue(f, &req, Some(id)).await?;
    f.store.set_decision_retry(decision, n.id)?;
    if old.workflow == "intake" && !old.plan.is_empty() {
        n.plan = old.plan.clone();
        f.store.update_task(&n)?;
    }
    Ok((decision, n))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn check_answer_scope_allows_anything_when_unscoped() {
        assert!(check_answer_scope(1, Some("a"), Some("alice"), None).is_ok());
        assert!(check_answer_scope(1, None, None, None).is_ok());
    }

    #[test]
    fn check_answer_scope_allows_a_matching_project_and_recipient() {
        assert!(
            check_answer_scope(1, Some("acme"), Some("alice"), Some(("acme", "alice"))).is_ok()
        );
    }

    #[test]
    fn check_answer_scope_refuses_a_task_in_a_different_project_naming_both() {
        let err = check_answer_scope(7, Some("acme"), Some("alice"), Some(("other", "alice")))
            .unwrap_err()
            .to_string();
        assert!(err.contains('7'), "{err}");
        assert!(err.contains("acme"), "{err}");
        assert!(err.contains("other"), "{err}");
    }

    #[test]
    fn check_answer_scope_refuses_a_question_addressed_to_someone_else_naming_both() {
        let err = check_answer_scope(7, Some("acme"), Some("bob"), Some(("acme", "alice")))
            .unwrap_err()
            .to_string();
        assert!(err.contains('7'), "{err}");
        assert!(err.contains("bob"), "{err}");
        assert!(err.contains("alice"), "{err}");
    }

    #[test]
    fn check_answer_scope_refuses_a_bare_question_for_a_named_recipient() {
        let err = check_answer_scope(7, Some("acme"), None, Some(("acme", "alice")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("operator"), "{err}");
        assert!(err.contains("alice"), "{err}");
    }
}
