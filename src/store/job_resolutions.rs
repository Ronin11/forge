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

    /// Close a `needs_human` or `failed` job by an operator's say-so, no
    /// rerun: one decision row (`kind` `job-dismiss`, no task) and one
    /// `dismissed` resolution, in one transaction. Refused for a job in
    /// any other state, and for one already closed.
    pub fn dismiss_job(&self, job_id: i64, reason: &str, by: &str) -> Result<i64> {
        let mut c = self.lock();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (project, state): (String, String) = tx
            .query_row(
                "SELECT project, state FROM jobs WHERE id=?1",
                params![job_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .with_context(|| format!("no job {job_id}"))?;
        if state != "needs_human" && state != "failed" {
            anyhow::bail!(
                "job {job_id} is {state}; only a needs_human or failed job can be dismissed"
            );
        }
        let now = crate::unix_now();
        let n = tx.execute(
            "INSERT OR IGNORE INTO job_resolutions (job_id, resolution, answer, answered_by, task_id, at)
             VALUES (?1, 'dismissed', ?2, ?3, NULL, ?4)",
            params![job_id, reason, by, now],
        )?;
        if n != 1 {
            anyhow::bail!("job {job_id} is already closed");
        }
        tx.execute(
            "INSERT INTO decisions (task_id, repo, question, answer, created_at, answered_by, citations, answered_for, kind)
             VALUES (NULL, ?1, ?2, ?3, ?4, ?5, '', NULL, 'job-dismiss')",
            params![project, format!("job {job_id} dismissed"), reason, now, by],
        )?;
        let decision = tx.last_insert_rowid();
        tx.commit()?;
        Ok(decision)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_job(state: &str) -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("forge.db")).unwrap();
        {
            let c = s.lock();
            c.execute(
                "INSERT INTO projects (name, purpose, created_at) VALUES ('forge', 'p', 0)",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO jobs (id, project, workflow, trigger_kind, trigger_ref, state, started_at, finished_at)
                 VALUES (45, 'forge', 'author-workflow', 'manual', 'x', ?1, 100, 100)",
                params![state],
            )
            .unwrap();
        }
        (d, s)
    }

    #[test]
    fn dismissing_a_needs_human_job_records_a_decision_and_closes_it_from_jobs_stale() {
        let (_d, s) = store_with_job("needs_human");
        assert_eq!(s.stale_jobs(1_000).unwrap().len(), 1);
        let id = s.dismiss_job(45, "abandoned draft", "operator").unwrap();
        let r = s.job_resolution(45).unwrap().unwrap();
        assert_eq!(r.resolution, "dismissed");
        assert_eq!(r.answer, "abandoned draft");
        let kind: String = s
            .lock()
            .query_row("SELECT kind FROM decisions WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kind, "job-dismiss");
        assert!(s.stale_jobs(1_000).unwrap().is_empty());
        let again = s.dismiss_job(45, "again", "operator").unwrap_err();
        assert!(format!("{again:#}").contains("already closed"), "{again:#}");
    }

    #[test]
    fn only_a_needs_human_or_failed_job_can_be_dismissed() {
        let (_d, s) = store_with_job("ok");
        let e = s.dismiss_job(45, "r", "operator").unwrap_err();
        assert!(
            format!("{e:#}").contains("only a needs_human or failed"),
            "{e:#}"
        );
        assert!(s.dismiss_job(99, "r", "operator").is_err());
    }
}
