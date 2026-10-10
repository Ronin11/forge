//! What `forge doctor`'s watches read (docs/OPS.md, "What doctor
//! watches"): the runs of each step kind, the checks every task ran, the
//! worker starts, the jobs left failed, and the little each watch keeps
//! between runs (`doctor_marks`, `doctor_samples`). Everything a watch
//! decides is computed from these rows, never from the journal, which
//! keeps a week and had already lost the supervisor's first failure.

use super::*;

/// One finished run of a step kind: an attempt at that step, or a kernel
/// operation (`verify`, `land`) by that name.
#[derive(Debug, Clone, PartialEq)]
pub struct StepRun {
    pub task_id: i64,
    pub ok: bool,
    pub reason: String,
    pub at: i64,
}

/// One finished attempt's verdict, with the task's project and branch:
/// what the environmental-check watch groups by.
#[derive(Debug, Clone)]
pub struct VerdictRow {
    pub task_id: i64,
    pub project: String,
    pub branch: String,
    pub verdict_json: String,
}

/// One registered worker start, with what was recorded about it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerStart {
    pub id: i64,
    pub pid: i64,
    pub started_at: i64,
    /// systemd's `NRestarts` for the unit at this start; `None` off systemd.
    pub restarts: Option<i64>,
    /// How the run before this one ended, as far as anything recorded it.
    pub exit_reason: String,
}

/// One per-attempt egress record: when it finished, its repository, and
/// its `audit::Outputs` JSON (the `refused` hosts are on it).
#[derive(Debug, Clone)]
pub struct RefusalRow {
    pub at: i64,
    pub repo: String,
    pub outputs_json: String,
}

/// A watch's remembered state (`doctor_marks`).
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub status: String,
    pub detail: String,
    pub at: i64,
}

impl Store {
    /// The last `n` finished runs of step kind `step`, newest first: its
    /// attempts (a succeeded attempt is a success) and its kernel ops of
    /// the same name (`ok` is a success), merged by time.
    pub fn step_runs(&self, step: &str, n: usize) -> Result<Vec<StepRun>> {
        let c = self.lock();
        let mut out = Vec::new();
        let mut stmt = c.prepare(
            "SELECT task_id, state, reason, COALESCE(finished_at, started_at) AS at FROM attempts
             WHERE step=?1 AND state != 'running' ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![step, n as i64], |r| {
            Ok(StepRun {
                task_id: r.get("task_id")?,
                ok: r.get::<_, String>("state")? == "succeeded",
                reason: r.get("reason")?,
                at: r.get("at")?,
            })
        })?;
        out.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        let mut stmt = c.prepare(
            "SELECT task_id, ok, detail, started_at + ms / 1000 AS at FROM ops
             WHERE name=?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![step, n as i64], |r| {
            Ok(StepRun {
                task_id: r.get("task_id")?,
                ok: r.get::<_, i64>("ok")? != 0,
                reason: r.get("detail")?,
                at: r.get("at")?,
            })
        })?;
        out.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        out.sort_by_key(|r| std::cmp::Reverse(r.at));
        out.truncate(n);
        Ok(out)
    }

    /// Every finished run of step kind `step` since `since`, newest first.
    pub fn step_runs_since(&self, step: &str, since: i64) -> Result<Vec<StepRun>> {
        let mut runs = self.step_runs(step, 10_000)?;
        runs.retain(|r| r.at >= since);
        Ok(runs)
    }

    /// How many tasks are blocked right now.
    pub fn blocked_count(&self) -> Result<i64> {
        Ok(self.lock().retry_query_row(
            "SELECT COUNT(*) FROM tasks WHERE state='blocked'",
            [],
            |r| r.get(0),
        )?)
    }

    /// Every attempt finished since `since` with a verdict, with its
    /// task's project (empty for none) and branch.
    pub fn verdicts_since(&self, since: i64) -> Result<Vec<VerdictRow>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT a.task_id, COALESCE(t.project, '') AS project, t.branch, a.verdict_json
             FROM attempts a JOIN tasks t ON t.id = a.task_id
             WHERE a.finished_at >= ?1 AND a.verdict_json != '[]'
             ORDER BY a.id",
        )?;
        let rows = stmt.query_map([since], |r| {
            Ok(VerdictRow {
                task_id: r.get(0)?,
                project: r.get(1)?,
                branch: r.get(2)?,
                verdict_json: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record what worker `id` learned at its start: the unit's restart
    /// count, and how the run before it ended.
    pub fn record_worker_start(
        &self,
        id: i64,
        restarts: Option<i64>,
        exit_reason: &str,
    ) -> Result<()> {
        self.lock().retry_execute(
            "UPDATE workers SET restarts=?2, exit_reason=?3 WHERE id=?1",
            params![id, restarts, exit_reason],
        )?;
        Ok(())
    }

    /// The newest worker row before `id`: whether it stopped cleanly
    /// (`stopped_at` set by `stop_worker`) or is still open, so a start
    /// can say how the run before it ended when systemd cannot.
    pub fn previous_worker(&self, id: i64) -> Result<Option<(i64, Option<i64>)>> {
        Ok(self
            .lock()
            .retry_query_row(
                "SELECT pid, stopped_at FROM workers WHERE id < ?1 ORDER BY id DESC LIMIT 1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Every worker start since `since`, oldest first.
    pub fn worker_starts_since(&self, since: i64) -> Result<Vec<WorkerStart>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT id, pid, started_at, restarts, exit_reason FROM workers
             WHERE started_at >= ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([since], |r| {
            Ok(WorkerStart {
                id: r.get(0)?,
                pid: r.get(1)?,
                started_at: r.get(2)?,
                restarts: r.get(3)?,
                exit_reason: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Jobs left `needs_human` or `failed` that finished before `before`:
    /// real runs only, not closed by an answer, and not followed by a
    /// later `ok` run of the same workflow in the same project.
    pub fn stale_jobs(&self, before: i64) -> Result<Vec<Job>> {
        let c = self.lock();
        let cols = jobs::JOB_COLUMNS.join(", ");
        let mut stmt = c.prepare(&format!(
            "SELECT {cols} FROM jobs j
             WHERE j.state IN ('needs_human', 'failed') AND j.dry_run = 0
               AND COALESCE(j.finished_at, j.started_at) < ?1
               AND NOT EXISTS (SELECT 1 FROM job_resolutions r WHERE r.job_id = j.id)
               AND NOT EXISTS (SELECT 1 FROM jobs k WHERE k.project = j.project
                   AND k.workflow = j.workflow AND k.id > j.id AND k.state = 'ok')
             ORDER BY j.id"
        ))?;
        let rows = stmt.query_map([before], jobs::job_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Per project, tasks finished since `since` and how many of them
    /// landed: agent work only, the same set `project_stats` counts.
    pub fn land_rates(&self, since: i64) -> Result<Vec<(String, i64, i64)>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT project, COUNT(*), SUM(landed_sha != '') FROM tasks
             WHERE project IS NOT NULL AND origin = 'agent' AND finished_at >= ?1
               AND state IN ('succeeded', 'failed', 'blocked', 'unverified', 'capped')
             GROUP BY project ORDER BY project",
        )?;
        let rows = stmt.query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Landed tasks whose worktree the record says is still on disk.
    pub fn landed_worktrees(&self) -> Result<Vec<(i64, String)>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT id, worktree FROM tasks WHERE landed_sha != '' AND worktree != ''
               AND worktree_removed_at IS NULL ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every attempt finished since `since` that recorded a refusal.
    pub fn refusals_since(&self, since: i64) -> Result<Vec<RefusalRow>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT a.finished_at, t.repo, a.outputs_json FROM attempts a
             JOIN tasks t ON t.id = a.task_id
             WHERE a.finished_at >= ?1 AND a.outputs_json LIKE '%\"refused\"%'
             ORDER BY a.id",
        )?;
        let rows = stmt.query_map([since], |r| {
            Ok(RefusalRow {
                at: r.get(0)?,
                repo: r.get(1)?,
                outputs_json: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Watch `name`'s remembered state.
    pub fn doctor_mark(&self, name: &str) -> Result<Option<Mark>> {
        Ok(self
            .lock()
            .retry_query_row(
                "SELECT status, detail, at FROM doctor_marks WHERE name=?1",
                [name],
                |r| {
                    Ok(Mark {
                        status: r.get(0)?,
                        detail: r.get(1)?,
                        at: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_doctor_mark(&self, name: &str, m: &Mark) -> Result<()> {
        self.lock().retry_execute(
            "INSERT INTO doctor_marks (name, status, detail, at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET status=?2, detail=?3, at=?4",
            params![name, m.status, m.detail, m.at],
        )?;
        Ok(())
    }

    /// Record today's sample of `kind`; a later sample the same day
    /// replaces it.
    pub fn record_doctor_sample(
        &self,
        kind: &str,
        day: &str,
        value: i64,
        extra: i64,
        at: i64,
    ) -> Result<()> {
        self.lock().retry_execute(
            "INSERT INTO doctor_samples (kind, day, value, extra, at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(kind, day) DO UPDATE SET value=?3, extra=?4, at=?5",
            params![kind, day, value, extra, at],
        )?;
        Ok(())
    }

    /// The last `n` days' samples of `kind`, oldest first: `(day, value,
    /// extra)`.
    pub fn doctor_samples(&self, kind: &str, n: usize) -> Result<Vec<(String, i64, i64)>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT day, value, extra FROM doctor_samples WHERE kind=?1 ORDER BY day DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![kind, n as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        out.reverse();
        Ok(out)
    }
}
