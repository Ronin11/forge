use super::*;

/// A job's state: a run workflow's run, the way `TaskState` is a build
/// workflow's (see docs/JOBS.md, "Vocabulary"). `NeedsHuman` is a job's
/// `on_failure = "ask:*"` outcome, the job analogue of `TaskState::Blocked`.
/// `Scheduled` is a job created with a due time (`Job::due_at`) still in the
/// future: it waits there, a row and never an in-memory timer, until the
/// worker's claim (`claim_next_job`) finds it due (docs/JOBS.md, "Delayed
/// jobs").
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum JobState {
    #[default]
    Queued,
    Scheduled,
    Running,
    Ok,
    Failed,
    NeedsHuman,
    Dropped,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Scheduled => "scheduled",
            JobState::Running => "running",
            JobState::Ok => "ok",
            JobState::Failed => "failed",
            JobState::NeedsHuman => "needs_human",
            JobState::Dropped => "dropped",
        }
    }
}

impl TryFrom<&str> for JobState {
    type Error = std::io::Error;
    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        Ok(match s {
            "queued" => JobState::Queued,
            "scheduled" => JobState::Scheduled,
            "running" => JobState::Running,
            "ok" => JobState::Ok,
            "failed" => JobState::Failed,
            "needs_human" => JobState::NeedsHuman,
            "dropped" => JobState::Dropped,
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown job state {other:?}"
                )));
            }
        })
    }
}

/// One run of a `kind = "run"` workflow (see docs/JOBS.md, "Vocabulary"):
/// its trigger, its pinned workflow version, its state, and its cost.
/// `job_steps` and `job_effects` carry what it did; this row is what
/// `forge job list`/`show` and `finish_job` read and write.
#[derive(Default, Debug, Clone)]
pub struct Job {
    pub id: i64,
    pub project: String,
    pub workflow: String,
    /// Content hash of the workflow file this job ran under, mirroring
    /// `Task::workflow_hash`.
    pub workflow_hash: String,
    /// The project's landed commit this job ran the workflow's automation
    /// files at; empty for a job whose project has never landed anything.
    pub landed_sha: String,
    /// `workflows::TriggerOn::as_str()`: `manual`, `schedule`, `message`,
    /// `webhook`, or `event`.
    pub trigger_kind: String,
    /// The trigger's own value (a cron string, a contact, a webhook name,
    /// an event type), mirroring `workflows::Trigger::value()`; empty for
    /// a manual trigger.
    pub trigger_ref: String,
    pub state: JobState,
    /// `workflows::JobSource::as_str()`: `"repo"` when the workflow came
    /// from the project's own repository at `landed_sha`, `"catalog"` when
    /// it fell back to the operator's catalog (docs/JOBS.md, "Where an
    /// automation lives").
    pub workflow_source: String,
    /// Effects recorded, not performed: `forge job test`'s replay mode
    /// (docs/JOBS.md, "Verifying an automation").
    pub dry_run: bool,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub cost_usd: Option<f64>,
    /// The assertions' verdict, in the same shape as a task attempt's
    /// `verdict_json` (`checks::CheckResult` rows); empty until the job
    /// finishes.
    pub verdict_json: String,
    /// When this job becomes claimable, a unix second; `None` for a job
    /// that was never delayed. Set from `forge job start --at`/`--delay`
    /// or from a `[trigger] delay` firing (docs/JOBS.md, "Delayed jobs").
    /// `JobState::Scheduled` while this is still in the future;
    /// `claim_next_job` is the only place that reads it against now.
    pub due_at: Option<i64>,
}

/// One step of a job's run: one entry of the workflow's `steps`, whether
/// it was an operation or a directive (see docs/JOBS.md, "Steps").
#[derive(Default, Debug, Clone)]
pub struct JobStep {
    pub id: i64,
    pub job_id: i64,
    /// Position in the workflow's `steps` array, from 0.
    pub seq: i64,
    /// The action's name, e.g. `"draft-quote"`.
    pub action: String,
    /// `"operation"` or `"directive"`.
    pub kind: String,
    /// Set only for a directive step: the role's provider.
    pub provider: String,
    /// Set only for a directive step: the model that ran it.
    pub model: String,
    pub cost_usd: Option<f64>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// Set only for an operation step.
    pub exit_code: Option<i32>,
    /// Where the step's output is on disk, relative to the job's scratch
    /// directory.
    pub output_ref: String,
}

/// One effect a job's step performed on the world (see docs/JOBS.md,
/// "Effects"): a message sent, a row written, a file produced, an HTTP
/// call made. Logged whether or not the run was a dry run.
#[derive(Default, Debug, Clone)]
pub struct JobEffect {
    pub id: i64,
    pub job_id: i64,
    /// The step's `seq` that produced this effect.
    pub seq: i64,
    /// The operation's declared effect kind, e.g. `"message"`, `"row"`.
    pub kind: String,
    /// What the effect acted on: a phone number, a table row, a URL.
    pub target: String,
    /// A short human-readable description, what the portal shows per run.
    pub summary: String,
    /// True when the effect was only recorded, not performed (a dry run,
    /// e.g. `forge job test`'s fixture replay).
    pub dry_run: bool,
}

/// Jobs run in the last rolling 24h for one project, by outcome: what
/// `forge project show` and `forge stats`'s jobs section count separately
/// from tasks (docs/JOBS.md step 1d). `today` is every job started in the
/// window, whatever its current state; `ok`/`failed`/`needs_human` are
/// those of them that reached that state (a still-`queued` or `running`
/// job counts toward `today` alone).
#[derive(Default, Debug, Clone)]
pub struct JobStat {
    pub project: String,
    pub today: i64,
    pub ok: i64,
    pub failed: i64,
    pub needs_human: i64,
}

pub(super) const JOB_COLUMNS: &[&str] = &[
    "id",
    "project",
    "workflow",
    "workflow_hash",
    "landed_sha",
    "trigger_kind",
    "trigger_ref",
    "state",
    "dry_run",
    "started_at",
    "finished_at",
    "cost_usd",
    "verdict_json",
    "workflow_source",
    "due_at",
];

pub(super) const JOB_STEP_COLUMNS: &[&str] = &[
    "id",
    "job_id",
    "seq",
    "action",
    "kind",
    "provider",
    "model",
    "cost_usd",
    "started_at",
    "finished_at",
    "exit_code",
    "output_ref",
];

pub(super) const JOB_EFFECT_COLUMNS: &[&str] = &[
    "id", "job_id", "seq", "kind", "target", "summary", "dry_run",
];

fn job_from_row(r: &Row) -> rusqlite::Result<Job> {
    Ok(Job {
        id: r.get("id")?,
        project: r.get("project")?,
        workflow: r.get("workflow")?,
        workflow_hash: r.get("workflow_hash")?,
        landed_sha: r.get("landed_sha")?,
        trigger_kind: r.get("trigger_kind")?,
        trigger_ref: r.get("trigger_ref")?,
        state: conv(
            r,
            "state",
            JobState::try_from(r.get::<_, String>("state")?.as_str()),
        )?,
        dry_run: r.get("dry_run")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        cost_usd: r.get("cost_usd")?,
        verdict_json: r.get("verdict_json")?,
        workflow_source: r.get("workflow_source")?,
        due_at: r.get("due_at")?,
    })
}

fn job_step_from_row(r: &Row) -> rusqlite::Result<JobStep> {
    Ok(JobStep {
        id: r.get("id")?,
        job_id: r.get("job_id")?,
        seq: r.get("seq")?,
        action: r.get("action")?,
        kind: r.get("kind")?,
        provider: r.get("provider")?,
        model: r.get("model")?,
        cost_usd: r.get("cost_usd")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        exit_code: r.get("exit_code")?,
        output_ref: r.get("output_ref")?,
    })
}

fn job_effect_from_row(r: &Row) -> rusqlite::Result<JobEffect> {
    Ok(JobEffect {
        id: r.get("id")?,
        job_id: r.get("job_id")?,
        seq: r.get("seq")?,
        kind: r.get("kind")?,
        target: r.get("target")?,
        summary: r.get("summary")?,
        dry_run: r.get("dry_run")?,
    })
}

impl Store {
    /// One project's jobs in the last rolling 24h, by outcome: what `forge
    /// project show` counts separately from its task rollup (see
    /// `JobStat`, docs/JOBS.md step 1d).
    pub fn project_job_stats(&self, project: &str, since: i64) -> Result<JobStat> {
        Ok(self.lock().query_row(
            "SELECT COUNT(*) AS today, SUM(state='ok') AS ok, SUM(state='failed') AS failed, SUM(state='needs_human') AS needs_human
             FROM jobs WHERE project=?1 AND started_at >= ?2",
            params![project, since],
            |r| {
                Ok(JobStat {
                    project: project.to_string(),
                    today: r.get("today")?,
                    ok: r.get::<_, Option<i64>>("ok")?.unwrap_or(0),
                    failed: r.get::<_, Option<i64>>("failed")?.unwrap_or(0),
                    needs_human: r.get::<_, Option<i64>>("needs_human")?.unwrap_or(0),
                })
            },
        )?)
    }

    /// Every project with a job in the last rolling 24h, by outcome: what
    /// `forge stats` adds as its jobs section when it is not itself scoped
    /// to one project or initiative (see `JobStat`).
    pub fn job_stats(&self, since: i64) -> Result<Vec<JobStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT project AS project, COUNT(*) AS today, SUM(state='ok') AS ok, SUM(state='failed') AS failed, SUM(state='needs_human') AS needs_human
             FROM jobs WHERE started_at >= ?1 GROUP BY project ORDER BY project",
        )?;
        let rows = stmt.query_map(params![since], |r| {
            Ok(JobStat {
                project: r.get("project")?,
                today: r.get("today")?,
                ok: r.get::<_, Option<i64>>("ok")?.unwrap_or(0),
                failed: r.get::<_, Option<i64>>("failed")?.unwrap_or(0),
                needs_human: r.get::<_, Option<i64>>("needs_human")?.unwrap_or(0),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record a job starting. Returns its id; `finish_job` completes it,
    /// `append_job_step`/`append_job_effect` record what it did along the
    /// way (see docs/JOBS.md, "The record"), and `src/job.rs` is the
    /// executor that calls all four.
    pub fn create_job(&self, j: &Job) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO jobs (project, workflow, workflow_hash, landed_sha, trigger_kind, trigger_ref, state, workflow_source, dry_run, started_at, finished_at, cost_usd, verdict_json, due_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                j.project,
                j.workflow,
                j.workflow_hash,
                j.landed_sha,
                j.trigger_kind,
                j.trigger_ref,
                j.state.as_str(),
                j.workflow_source,
                j.dry_run,
                j.started_at,
                j.finished_at,
                j.cost_usd,
                j.verdict_json,
                j.due_at,
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Record one step of a job's run. Returns its id; see `create_job`.
    pub fn append_job_step(&self, s: &JobStep) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO job_steps (job_id, seq, action, kind, provider, model, cost_usd, started_at, finished_at, exit_code, output_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                s.job_id,
                s.seq,
                s.action,
                s.kind,
                s.provider,
                s.model,
                s.cost_usd,
                s.started_at,
                s.finished_at,
                s.exit_code,
                s.output_ref,
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Record one effect a job's step performed on the world. Returns its
    /// id; see `create_job`.
    pub fn append_job_effect(&self, e: &JobEffect) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO job_effects (job_id, seq, kind, target, summary, dry_run)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![e.job_id, e.seq, e.kind, e.target, e.summary, e.dry_run],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Record a job's outcome: its final state, cost and assertions'
    /// verdict; see `create_job`.
    pub fn finish_job(
        &self,
        id: i64,
        at: i64,
        state: JobState,
        cost_usd: Option<f64>,
        verdict_json: &str,
    ) -> Result<()> {
        self.lock().execute(
            "UPDATE jobs SET state=?2, finished_at=?3, cost_usd=?4, verdict_json=?5 WHERE id=?1",
            params![id, state.as_str(), at, cost_usd, verdict_json],
        )?;
        Ok(())
    }

    /// One job by id.
    pub fn job(&self, id: i64) -> Result<Option<Job>> {
        Ok(self
            .lock()
            .query_row(
                &format!("SELECT {} FROM jobs WHERE id=?1", JOB_COLUMNS.join(", ")),
                params![id],
                job_from_row,
            )
            .optional()?)
    }

    /// Jobs, newest first, optionally narrowed to one project and/or one
    /// state: what `forge job list` shows.
    pub fn jobs(&self, project: Option<&str>, state: Option<JobState>) -> Result<Vec<Job>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM jobs WHERE (?1 IS NULL OR project=?1) AND (?2 IS NULL OR state=?2) ORDER BY id DESC",
            JOB_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project, state.map(JobState::as_str)], job_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// One job's steps, in the order they ran.
    pub fn job_steps(&self, job_id: i64) -> Result<Vec<JobStep>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM job_steps WHERE job_id=?1 ORDER BY seq",
            JOB_STEP_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![job_id], job_step_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// One job's effects, in the order they happened.
    pub fn job_effects(&self, job_id: i64) -> Result<Vec<JobEffect>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM job_effects WHERE job_id=?1 ORDER BY seq",
            JOB_EFFECT_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![job_id], job_effect_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// A project's job effects across every one of its jobs, newest first:
    /// what `forge job log` shows.
    pub fn job_effects_for_project(&self, project: &str) -> Result<Vec<JobEffect>> {
        let c = self.lock();
        let cols = JOB_EFFECT_COLUMNS
            .iter()
            .map(|c| format!("job_effects.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = c.prepare(&format!(
            "SELECT {cols}
             FROM job_effects JOIN jobs ON jobs.id = job_effects.job_id
             WHERE jobs.project=?1 ORDER BY job_effects.id DESC",
        ))?;
        let rows = stmt.query_map(params![project], job_effect_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Queued jobs, oldest first: what the worker's claim loop considers,
    /// alongside `queued_unblocked`'s tasks (see `claim_next_job`).
    pub fn queued_jobs(&self) -> Result<Vec<Job>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM jobs WHERE state='queued' ORDER BY id",
            JOB_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map([], job_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The oldest claimable job the worker can claim right now, same shape
    /// as `claim_next` for tasks: a job carries no provider or initiative
    /// hold yet (it runs no directive step), so the first one found is
    /// always claimable. A `queued` job is always claimable; a `scheduled`
    /// one only once its `due_at` is at or before now (docs/JOBS.md,
    /// "Delayed jobs") — the wait is this one condition on a row, never an
    /// in-memory timer, so it survives a worker restart.
    pub fn claim_next_job(&self) -> Result<Option<Job>> {
        let id: Option<i64> = self
            .lock()
            .query_row(
                "UPDATE jobs SET state='running'
                 WHERE id = (
                   SELECT id FROM jobs
                   WHERE state='queued' OR (state='scheduled' AND due_at <= ?1)
                   ORDER BY id LIMIT 1
                 )
                 RETURNING id",
                params![crate::unix_now()],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.job(id),
            None => Ok(None),
        }
    }

    /// Withdraw a scheduled job: the operator decided it should not run
    /// after all, before it ever became claimable (docs/JOBS.md, "Delayed
    /// jobs"). Atomic on state, so a job the worker claims in between
    /// (its `due_at` having just passed) is left alone. Returns whether it
    /// changed anything.
    pub fn withdraw_job(&self, id: i64) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE jobs SET state='dropped', finished_at=?2 WHERE id=?1 AND state='scheduled'",
            params![id, crate::unix_now()],
        )?;
        Ok(n == 1)
    }

    /// How many of `project`'s runs of `workflow` started at or after
    /// `since`, dry runs excluded: `Limits.per_day` is checked against it
    /// by `job::start` (docs/JOBS.md, "Limits").
    pub fn jobs_started_since(&self, project: &str, workflow: &str, since: i64) -> Result<i64> {
        Ok(self.lock().query_row(
            "SELECT COUNT(*) FROM jobs WHERE project=?1 AND workflow=?2 AND dry_run=0 AND started_at >= ?3",
            params![project, workflow, since],
            |r| r.get(0),
        )?)
    }

    /// The latest slot (`Job::trigger_ref`, a unix second) `project`'s
    /// `workflow` has already started a `schedule`-triggered job for, or
    /// `None` if it never has: what the worker's schedule tick
    /// (`src/worker.rs`) compares a cron's due slots against so the same
    /// slot never starts twice and a restart cannot double-fire (docs/JOBS.md,
    /// "Triggers").
    pub fn last_scheduled_job(&self, project: &str, workflow: &str) -> Result<Option<i64>> {
        Ok(self.lock().query_row(
            "SELECT MAX(CAST(trigger_ref AS INTEGER)) FROM jobs WHERE project=?1 AND workflow=?2 AND trigger_kind='schedule'",
            params![project, workflow],
            |r| r.get(0),
        )?)
    }

    /// Put a running job back in the queue: the worker aborted with it
    /// still in flight (see `worker::work`'s double-signal abort, which
    /// does the same for a running task's `requeue`).
    pub fn requeue_job(&self, id: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE jobs SET state='queued' WHERE id=?1 AND state='running'",
            params![id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_project(s: &Store, name: &str) {
        s.create_project(&Project {
            name: name.to_string(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    }

    fn mk_job(s: &Store, project: &str, workflow: &str, started_at: i64) -> i64 {
        s.create_job(&Job {
            project: project.into(),
            workflow: workflow.into(),
            workflow_hash: "deadbeef".into(),
            landed_sha: "cafef00d".into(),
            trigger_kind: "manual".into(),
            trigger_ref: "".into(),
            state: JobState::Running,
            dry_run: false,
            started_at,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn a_created_job_is_read_back_with_its_state_round_tripped() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        assert!(s.job(1).unwrap().is_none());

        let id = mk_job(&s, "equitizr", "quote-by-text", 100);
        let j = s.job(id).unwrap().unwrap();
        assert_eq!(j.project, "equitizr");
        assert_eq!(j.workflow, "quote-by-text");
        assert_eq!(j.workflow_hash, "deadbeef");
        assert_eq!(j.landed_sha, "cafef00d");
        assert_eq!(j.trigger_kind, "manual");
        assert_eq!(j.state, JobState::Running);
        assert!(!j.dry_run);
        assert_eq!(j.started_at, 100);
        assert!(j.finished_at.is_none());
        assert!(j.cost_usd.is_none());
    }

    #[test]
    fn finish_job_sets_state_cost_and_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        let id = mk_job(&s, "equitizr", "quote-by-text", 100);

        s.finish_job(
            id,
            130,
            JobState::Ok,
            Some(0.02),
            r#"[{"level":"L0","name":"quoted","ok":true}]"#,
        )
        .unwrap();

        let j = s.job(id).unwrap().unwrap();
        assert_eq!(j.state, JobState::Ok);
        assert_eq!(j.finished_at, Some(130));
        assert_eq!(j.cost_usd, Some(0.02));
        assert!(j.verdict_json.contains("quoted"));
    }

    #[test]
    fn jobs_lists_newest_first_and_filters_by_project_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        mk_project(&s, "nucleosynthesis");

        let a = mk_job(&s, "equitizr", "quote-by-text", 100);
        let b = mk_job(&s, "equitizr", "quote-by-text", 200);
        let c = mk_job(&s, "nucleosynthesis", "other", 300);
        s.finish_job(b, 250, JobState::Failed, None, "[]").unwrap();

        let all = s.jobs(None, None).unwrap();
        assert_eq!(
            all.iter().map(|j| j.id).collect::<Vec<_>>(),
            vec![c, b, a],
            "newest first"
        );

        let equitizr_only = s.jobs(Some("equitizr"), None).unwrap();
        assert_eq!(
            equitizr_only.iter().map(|j| j.id).collect::<Vec<_>>(),
            vec![b, a]
        );

        let failed_only = s.jobs(None, Some(JobState::Failed)).unwrap();
        assert_eq!(failed_only.len(), 1);
        assert_eq!(failed_only[0].id, b);

        let equitizr_running = s.jobs(Some("equitizr"), Some(JobState::Running)).unwrap();
        assert_eq!(equitizr_running.len(), 1);
        assert_eq!(equitizr_running[0].id, a);
    }

    #[test]
    fn claim_next_job_skips_a_scheduled_job_whose_due_at_is_in_the_future() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        let now = crate::unix_now();
        let id = s
            .create_job(&Job {
                project: "equitizr".into(),
                workflow: "quote-by-text".into(),
                trigger_kind: "manual".into(),
                state: JobState::Scheduled,
                due_at: Some(now + 3600),
                started_at: now,
                ..Default::default()
            })
            .unwrap();

        assert!(s.claim_next_job().unwrap().is_none());
        let j = s.job(id).unwrap().unwrap();
        assert_eq!(j.state, JobState::Scheduled, "left alone: not due yet");
    }

    #[test]
    fn claim_next_job_claims_a_scheduled_job_once_its_due_at_has_passed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        let now = crate::unix_now();
        let id = s
            .create_job(&Job {
                project: "equitizr".into(),
                workflow: "quote-by-text".into(),
                trigger_kind: "manual".into(),
                state: JobState::Scheduled,
                due_at: Some(now - 60),
                started_at: now,
                ..Default::default()
            })
            .unwrap();

        let claimed = s.claim_next_job().unwrap().unwrap();
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.state, JobState::Running);
    }

    #[test]
    fn claim_next_job_still_claims_a_plain_queued_job_with_no_due_at() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        let id = s
            .create_job(&Job {
                project: "equitizr".into(),
                workflow: "quote-by-text".into(),
                trigger_kind: "manual".into(),
                state: JobState::Queued,
                started_at: crate::unix_now(),
                ..Default::default()
            })
            .unwrap();

        let claimed = s.claim_next_job().unwrap().unwrap();
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.state, JobState::Running);
    }

    #[test]
    fn withdraw_job_drops_a_scheduled_job_but_refuses_any_other_state() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        let now = crate::unix_now();
        let scheduled = s
            .create_job(&Job {
                project: "equitizr".into(),
                workflow: "quote-by-text".into(),
                trigger_kind: "manual".into(),
                state: JobState::Scheduled,
                due_at: Some(now + 3600),
                started_at: now,
                ..Default::default()
            })
            .unwrap();
        let running = mk_job(&s, "equitizr", "quote-by-text", now);

        assert!(!s.withdraw_job(running).unwrap());
        assert_eq!(s.job(running).unwrap().unwrap().state, JobState::Running);

        assert!(s.withdraw_job(scheduled).unwrap());
        let j = s.job(scheduled).unwrap().unwrap();
        assert_eq!(j.state, JobState::Dropped);
        assert!(j.finished_at.is_some());

        assert!(!s.withdraw_job(scheduled).unwrap(), "already dropped");
    }

    #[test]
    fn job_steps_and_effects_are_recorded_and_read_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        let id = mk_job(&s, "equitizr", "quote-by-text", 100);

        assert!(s.job_steps(id).unwrap().is_empty());
        assert!(s.job_effects(id).unwrap().is_empty());

        s.append_job_step(&JobStep {
            job_id: id,
            seq: 0,
            action: "extract-job".into(),
            kind: "directive".into(),
            provider: "anthropic".into(),
            model: "haiku".into(),
            cost_usd: Some(0.001),
            started_at: 100,
            finished_at: Some(101),
            output_ref: "step-0.json".into(),
            ..Default::default()
        })
        .unwrap();
        s.append_job_step(&JobStep {
            job_id: id,
            seq: 1,
            action: "send-quote".into(),
            kind: "operation".into(),
            started_at: 101,
            finished_at: Some(102),
            exit_code: Some(0),
            output_ref: "step-1.json".into(),
            ..Default::default()
        })
        .unwrap();

        s.append_job_effect(&JobEffect {
            job_id: id,
            seq: 1,
            kind: "message".into(),
            target: "+15555550100".into(),
            summary: "quoted the Hendersons' fence job at $1,240".into(),
            dry_run: false,
            ..Default::default()
        })
        .unwrap();

        let steps = s.job_steps(id).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].action, "extract-job");
        assert_eq!(steps[0].kind, "directive");
        assert_eq!(steps[0].provider, "anthropic");
        assert_eq!(steps[1].action, "send-quote");
        assert_eq!(steps[1].exit_code, Some(0));

        let effects = s.job_effects(id).unwrap();
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].kind, "message");
        assert_eq!(effects[0].target, "+15555550100");
        assert!(!effects[0].dry_run);
    }

    #[test]
    fn job_effects_for_project_spans_every_job_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        mk_project(&s, "nucleosynthesis");
        let a = mk_job(&s, "equitizr", "quote-by-text", 100);
        let b = mk_job(&s, "equitizr", "quote-by-text", 200);
        let c = mk_job(&s, "nucleosynthesis", "other", 300);

        s.append_job_effect(&JobEffect {
            job_id: a,
            seq: 0,
            kind: "message".into(),
            target: "customer-a".into(),
            summary: "first".into(),
            dry_run: false,
            ..Default::default()
        })
        .unwrap();
        s.append_job_effect(&JobEffect {
            job_id: b,
            seq: 0,
            kind: "row".into(),
            target: "book".into(),
            summary: "second".into(),
            dry_run: true,
            ..Default::default()
        })
        .unwrap();
        s.append_job_effect(&JobEffect {
            job_id: c,
            seq: 0,
            kind: "message".into(),
            target: "customer-c".into(),
            summary: "other project".into(),
            dry_run: false,
            ..Default::default()
        })
        .unwrap();

        let effects = s.job_effects_for_project("equitizr").unwrap();
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0].summary, "second", "newest first");
        assert_eq!(effects[0].job_id, b);
        assert_eq!(effects[1].summary, "first");
        assert_eq!(effects[1].job_id, a);
    }
}
