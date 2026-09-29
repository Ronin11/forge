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
