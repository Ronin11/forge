use super::*;
use serde::Serialize;

/// How a job's `needs_human` was closed: today only `answered`, by the
/// answer to the job's `job question` task (docs/JOBS.md, "The human
/// rung, per run").
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JobResolution {
    pub job_id: i64,
    pub resolution: String,
    pub answer: String,
    pub answered_by: String,
    pub task_id: Option<i64>,
    pub at: i64,
}

impl Store {
    /// Record how `job_id`'s `needs_human` closed. A job is closed once:
    /// a second call leaves the first record and returns `false`.
    pub fn resolve_job(&self, r: &JobResolution) -> Result<bool> {
        let n = self.lock().retry_execute(
            "INSERT OR IGNORE INTO job_resolutions (job_id, resolution, answer, answered_by, task_id, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![r.job_id, r.resolution, r.answer, r.answered_by, r.task_id, r.at],
        )?;
        Ok(n == 1)
    }

    /// The record closing `job_id`'s `needs_human`, if it was closed.
    pub fn job_resolution(&self, job_id: i64) -> Result<Option<JobResolution>> {
        Ok(self
            .lock()
            .retry_query_row(
                "SELECT job_id, resolution, answer, answered_by, task_id, at FROM job_resolutions WHERE job_id=?1",
                params![job_id],
                |r| {
                    Ok(JobResolution {
                        job_id: r.get("job_id")?,
                        resolution: r.get("resolution")?,
                        answer: r.get("answer")?,
                        answered_by: r.get("answered_by")?,
                        task_id: r.get("task_id")?,
                        at: r.get("at")?,
                    })
                },
            )
            .optional()?)
    }

    /// Atomically close a no-work question and record its answer. Deploy
    /// questions are withdrawn; job questions retain their succeeded state.
    pub fn answer_blocked_question(&self, args: InsertDecisionBy<'_>, reason: &str) -> Result<i64> {
        let mut c = self.lock();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = crate::unix_now();
        let n = tx.retry_execute(
            "UPDATE tasks SET state=CASE WHEN deploy_id IS NULL THEN 'succeeded' ELSE 'withdrawn' END,
             reason=?2, finished_at=?3 WHERE id=?1 AND state='blocked'
             AND NOT EXISTS (SELECT 1 FROM attempts WHERE task_id=?1)",
            params![args.task_id, reason, now],
        )?;
        if n != 1 {
            anyhow::bail!(
                "task {} changed state before it could be answered",
                args.task_id
            );
        }
        tx.retry_execute(
            "INSERT INTO decisions (task_id, repo, question, answer, created_at, answered_by, citations, answered_for, retry_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?1)",
            params![args.task_id, args.repo, args.question, args.answer, now, args.answered_by, args.citations, args.answered_for],
        )?;
        let decision = tx.last_insert_rowid();
        tx.commit()?;
        drop(c);
        if let Some(task) = self.task(args.task_id)? {
            crate::disk::discard_task_caches(&task.worktree);
        }
        Ok(decision)
    }
}
