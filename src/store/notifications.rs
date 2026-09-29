//! Durable daily notification counts: one follow-up announcement per source
//! task, irrespective of how many events a plugin replayed.
use super::*;

impl Store {
    pub fn notification_digest(&self, start: i64, end: i64) -> Result<String> {
        let c = self.lock();
        let count = |predicate: &str| -> Result<i64> {
            Ok(c.query_row(
                &format!(
                    "SELECT count(DISTINCT d.task_id) FROM decisions d
                    JOIN tasks t ON t.id=d.task_id
                    JOIN tasks followup ON followup.id=d.retry_id
                    WHERE d.created_at>=?1 AND d.created_at<?2 AND {predicate}"
                ),
                params![start, end],
                |r| r.get(0),
            )?)
        };
        let demotions = count("d.kind='demotion-as-task'")?;
        let failures = count("d.kind LIKE 'mechanic-%'")?;
        let questions = count("d.kind='' AND d.answered_by NOT IN ('forge','mechanic')")?;
        let superseded: i64 = c.query_row(
            "SELECT count(DISTINCT task_id) FROM decisions WHERE created_at>=?1
             AND created_at<?2 AND answered_by='forge' AND answer LIKE 'superseded by %'
             AND task_id NOT IN (SELECT task_id FROM decisions WHERE kind='demotion-as-task')",
            params![start, end],
            |r| r.get(0),
        )?;
        Ok(format!(
            "yesterday: {demotions} demotions followed up, {failures} failures retried, {questions} questions answered, {superseded} blocks superseded"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answered_job_question_counts_without_creating_a_retry() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("db")).unwrap();
        let mut question = Task {
            task: "job question".into(),
            state: TaskState::Blocked,
            ..Default::default()
        };
        question.id = s.insert_task(&question).unwrap();
        s.update_task(&question).unwrap();
        let start = crate::unix_now();
        s.answer_blocked_question(
            InsertDecisionBy {
                task_id: question.id,
                repo: "",
                question: "resend the report?",
                answer: "skip this report",
                answered_by: "operator",
                citations: "",
                answered_for: None,
            },
            "skip this report",
        )
        .unwrap();
        assert_eq!(
            s.task(question.id).unwrap().unwrap().state,
            TaskState::Succeeded
        );
        assert!(s.live_descendants(question.id).unwrap().is_empty());
        assert_eq!(
            s.notification_digest(start, crate::unix_now() + 1).unwrap(),
            "yesterday: 0 demotions followed up, 0 failures retried, 1 questions answered, 0 blocks superseded"
        );
    }

    #[test]
    fn digest_counts_completed_actions_once_in_the_previous_day() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("db")).unwrap();
        let task = s.insert_task(&Task::default()).unwrap();
        let next = s.insert_task(&Task::default()).unwrap();
        let record = |kind: &str, at: i64, retry: Option<i64>| {
            s.lock()
                .execute(
                    "INSERT INTO decisions (task_id, repo, question, answer, created_at,
                 answered_by, citations, kind, retry_id) VALUES (?1, '', 'q', 'a', ?2,
                 'supervisor', '', ?3, ?4)",
                    params![task, at, kind, retry],
                )
                .unwrap();
        };
        record("demotion-as-task", 86_400, Some(next));
        record("demotion-as-task", 86_401, Some(next));
        record("mechanic-ratchet", 86_410, Some(next));
        record("", 86_420, Some(next));
        record("mechanic-clean-tree", 86_450, None); // filing failed
        assert_eq!(
            s.notification_digest(86_400, 172_800).unwrap(),
            "yesterday: 1 demotions followed up, 1 failures retried, 1 questions answered, 0 blocks superseded"
        );
        assert_eq!(
            s.notification_digest(172_800, 259_200).unwrap(),
            "yesterday: 0 demotions followed up, 0 failures retried, 0 questions answered, 0 blocks superseded"
        );
        assert_eq!(
            s.notification_digest(0, 86_400).unwrap(),
            "yesterday: 0 demotions followed up, 0 failures retried, 0 questions answered, 0 blocks superseded"
        );
    }
}
