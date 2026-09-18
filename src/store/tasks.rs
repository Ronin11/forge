use super::*;

/// The shared decision behind `release_dependents` and
/// `release_dependents_of`: for each `(id, after_json)` candidate — always
/// a task currently `blocked` with a reason starting "waits on task" —
/// walk its after list and either release it to `queued` with its reason
/// cleared (every dependency landed or was withdrawn: never a defect in
/// the work, see `TaskState::Withdrawn`), give it a fresh reason naming
/// the first dependency that ended badly (failed, unverified, or
/// succeeded without landing), or leave it alone (a dependency still
/// queued, running, or itself blocked has not resolved yet). Returns the
/// ids released to `queued`.
fn release_or_reblock(c: &Connection, candidates: Vec<(i64, String)>) -> Result<Vec<i64>> {
    let mut released = Vec::new();
    for (id, after_json) in candidates {
        let after: Vec<i64> = serde_json::from_str(&after_json).unwrap_or_default();
        let mut blocker: Option<(i64, String, String)> = None;
        let mut all_resolved = true;
        for d in after {
            let row: Option<(String, String, bool, String)> = c
                .query_row(
                    "SELECT state, reason, land, landed_sha FROM tasks WHERE id=?1",
                    params![d],
                    |r| {
                        Ok((
                            r.get("state")?,
                            r.get("reason")?,
                            r.get("land")?,
                            r.get("landed_sha")?,
                        ))
                    },
                )
                .optional()?;
            let Some((state, reason, land, landed_sha)) = row else {
                all_resolved = false;
                continue;
            };
            let ok =
                state == "withdrawn" || (state == "succeeded" && (!land || !landed_sha.is_empty()));
            if ok {
                continue;
            }
            all_resolved = false;
            if blocker.is_none() && matches!(state.as_str(), "failed" | "unverified" | "succeeded")
            {
                blocker = Some((d, state, reason));
            }
        }
        if let Some((d, state, reason)) = blocker {
            let why = format!("waits on task {d} ({state}: {reason})");
            c.execute(
                "UPDATE tasks SET reason=?2 WHERE id=?1 AND state='blocked'",
                params![id, why],
            )?;
        } else if all_resolved {
            c.execute(
                "UPDATE tasks SET state='queued', reason='', finished_at=NULL WHERE id=?1 AND state='blocked'",
                params![id],
            )?;
            released.push(id);
        }
    }
    Ok(released)
}

impl Store {
    pub fn insert_task(&self, t: &Task) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO tasks (repo, task, title, base_branch, model, provider, max_turns, max_attempts, timeout_secs, checks_json,
                                state, created_at, budget_usd, allow_protected, workflow, show_checks, workflow_hash, workflow_text, land, after_json, retry_of, journal, context_enabled, resume_on_failure, journal_arm, explore_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
            params![
                t.repo,
                t.task,
                t.title,
                t.base_branch,
                t.model,
                t.provider,
                t.max_turns,
                t.max_attempts,
                t.timeout_secs,
                serde_json::to_string(&t.checks)?,
                t.state.as_str(),
                t.created_at,
                t.budget_usd,
                t.allow_protected as i64,
                t.workflow,
                t.show_checks as i64,
                t.workflow_hash,
                t.workflow_text,
                t.land as i64,
                serde_json::to_string(&t.after)?,
                t.retry_of,
                t.journal as i64,
                t.context_enabled as i64,
                t.resume_on_failure as i64,
                t.journal_arm,
                serde_json::to_string(&t.explore)?,
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Persist every column the struct carries, except the id, the
    /// creation time, and `worktree_removed_at`, which gc owns. A field
    /// mutated after insert used to be silently dropped here.
    pub fn update_task(&self, t: &Task) -> Result<()> {
        self.lock().execute(
            "UPDATE tasks SET repo=?2, task=?3, base_branch=?4, base_sha=?5, branch=?6, worktree=?7, model=?8,
             max_turns=?9, max_attempts=?10, timeout_secs=?11, checks_json=?12, state=?13, reason=?14,
             started_at=?15, finished_at=?16, pushed=?17, worker_pid=?18, budget_usd=?19, allow_protected=?20,
             workflow=?21, workflow_hash=?22, workflow_text=?23, actions_json=?24, interface=?25, show_checks=?26,
             land=?27, after_json=?28, verify_base=?29, retry_of=?30, journal=?31, context=?32,
             context_enabled=?33, resume_on_failure=?34, plan=?35, landed_sha=?36, journal_arm=?37,
             project=?38, initiative=?39, provider=?40, question_to=?41, explore_json=?42,
             concierge_json=?43, proposal_json=?44, proposal_answer=?45, proposal_initiative=?46,
             title=?47, landed_at=?48, hand_landed=?49 WHERE id=?1",
            params![
                t.id,
                t.repo,
                t.task,
                t.base_branch,
                t.base_sha,
                t.branch,
                t.worktree,
                t.model,
                t.max_turns,
                t.max_attempts,
                t.timeout_secs,
                serde_json::to_string(&t.checks)?,
                t.state.as_str(),
                t.reason,
                t.started_at,
                t.finished_at,
                t.pushed as i64,
                t.worker_pid,
                t.budget_usd,
                t.allow_protected as i64,
                t.workflow,
                t.workflow_hash,
                t.workflow_text,
                t.actions_json,
                t.interface,
                t.show_checks as i64,
                t.land as i64,
                serde_json::to_string(&t.after)?,
                t.verify_base,
                t.retry_of,
                t.journal as i64,
                t.context,
                t.context_enabled as i64,
                t.resume_on_failure as i64,
                t.plan,
                t.landed_sha,
                t.journal_arm,
                t.project,
                t.initiative,
                t.provider,
                t.question_to,
                serde_json::to_string(&t.explore)?,
                t.concierge_json,
                t.proposal_json,
                t.proposal_answer,
                t.proposal_initiative,
                t.title,
                t.landed_at,
                t.hand_landed as i64,
            ],
        )?;
        Ok(())
    }

    pub fn task(&self, id: i64) -> Result<Option<Task>> {
        Ok(self
            .lock()
            .query_row(
                &format!("SELECT {} FROM tasks WHERE id=?1", TASK_COLUMNS.join(", ")),
                params![id],
                task_from_row,
            )
            .optional()?)
    }

    /// Queued tasks whose dependencies have all landed (or succeeded
    /// without landing, when they were told not to) and whose initiative
    /// is not in `held`, oldest first: what `claim_next` considers.
    pub fn queued_unblocked(&self, held: &[i64]) -> Result<Vec<Task>> {
        let ids: Vec<i64> = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT t.id FROM tasks t WHERE t.state='queued' AND NOT EXISTS (
                   SELECT 1 FROM json_each(t.after_json) j LEFT JOIN tasks d ON d.id = j.value
                   WHERE d.id IS NULL OR d.state != 'succeeded' OR (d.land = 1 AND d.landed_sha = '')
                 ) ORDER BY t.id",
            )?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut out = Vec::new();
        for id in ids {
            let Some(t) = self.task(id)? else { continue };
            if !t.initiative.is_some_and(|i| held.contains(&i)) {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Atomically take the oldest queued task for this worker, skipping
    /// any whose initiative is in `held` (the caller has already found
    /// those initiatives are holding new claims, see
    /// `view::initiative_hold`) or for which `provider_held` says the
    /// provider it would run under is at its rate-window cap: the oldest
    /// queued, unheld task whose dependencies have all landed.
    pub fn claim_next(
        &self,
        pid: i64,
        held: &[i64],
        provider_held: impl Fn(&Task) -> bool,
    ) -> Result<Option<Task>> {
        for t in self.queued_unblocked(held)? {
            if provider_held(&t) {
                continue;
            }
            if self.claim(t.id, pid)? {
                return self.task(t.id);
            }
        }
        Ok(None)
    }

    /// Atomically take one specific queued task.
    pub fn claim(&self, id: i64, pid: i64) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE tasks SET state='running', worker_pid=?2, started_at=?3 WHERE id=?1 AND state='queued'",
            params![id, pid, crate::unix_now()],
        )?;
        Ok(n == 1)
    }

    /// Withdraw a blocked or queued task: the operator decided it should
    /// not be done. Atomic on state, so a task the worker claims in
    /// between is left alone. Returns whether it changed anything.
    pub fn withdraw(&self, id: i64, reason: &str) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE tasks SET state='withdrawn', reason=?2, finished_at=?3 WHERE id=?1 AND state IN ('blocked', 'queued')",
            params![id, reason, crate::unix_now()],
        )?;
        Ok(n == 1)
    }

    /// Block every queued task that waits on a task which ended without
    /// landing. Returns the (dependent, dependency) pairs it blocked.
    pub fn block_dependents(&self) -> Result<Vec<(i64, i64, String)>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.id AS t_id, d.id AS d_id, d.state AS d_state, d.reason AS d_reason
             FROM tasks t, json_each(t.after_json) j JOIN tasks d ON d.id = j.value
             WHERE t.state='queued' AND d.state IN ('failed', 'unverified', 'withdrawn')
                OR (t.state='queued' AND d.state='succeeded' AND d.land = 1 AND d.landed_sha = '' AND d.finished_at IS NOT NULL)
             ORDER BY t.id, d.id",
        )?;
        let rows: Vec<(i64, i64, String, String)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get("t_id")?,
                    r.get("d_id")?,
                    r.get("d_state")?,
                    r.get("d_reason")?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::new();
        for (t, d, state, reason) in rows {
            let why = format!("waits on task {d} ({state}: {reason})");
            let n = c.execute(
                "UPDATE tasks SET state='blocked', reason=?2, finished_at=?3 WHERE id=?1 AND state='queued'",
                params![t, why, crate::unix_now()],
            )?;
            if n > 0 {
                out.push((t, d, why));
            }
        }
        Ok(out)
    }

    /// A retry of `old` carries its dependents along: every task waiting
    /// on `old` waits on `new` instead. Releasing a dependent that was
    /// swept into blocked by `old`'s failure is no longer this function's
    /// job: it happens once `new` itself reaches a terminal state, the
    /// same as any other re-pointed dependency (see
    /// `release_dependents_of`). Returns the ids moved.
    pub fn reroute_dependents(&self, old: i64, new: i64) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.id, t.after_json FROM tasks t, json_each(t.after_json) j
             WHERE j.value = ?1 AND t.state IN ('queued', 'blocked') AND t.id != ?2",
        )?;
        let rows: Vec<(i64, String)> = stmt
            .query_map(params![old, new], |r| {
                Ok((r.get("id")?, r.get("after_json")?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut moved = Vec::new();
        for (id, after_json) in rows {
            let after: Vec<i64> = serde_json::from_str(&after_json).unwrap_or_default();
            let after: Vec<i64> = after
                .into_iter()
                .map(|d| if d == old { new } else { d })
                .collect();
            c.execute(
                "UPDATE tasks SET after_json=?2 WHERE id=?1",
                params![id, serde_json::to_string(&after)?],
            )?;
            moved.push(id);
        }
        Ok(moved)
    }

    /// Re-evaluate every task that waits on `dep` and is currently blocked
    /// with a reason that says so: the trigger fired whenever a task
    /// reaches a terminal state (see `engine::run_task` and
    /// `queue::withdraw`), so a dependent whose after list was re-pointed
    /// at `dep` — by a retry's reroute or by hand — is released (or freshly
    /// reblocked) as soon as `dep` resolves, rather than waiting for the
    /// next full scan. Returns the ids released to `queued`.
    pub fn release_dependents_of(&self, dep: i64) -> Result<Vec<i64>> {
        let c = self.lock();
        let candidates: Vec<(i64, String)> = {
            let mut stmt = c.prepare(
                "SELECT t.id, t.after_json FROM tasks t, json_each(t.after_json) j
                 WHERE j.value = ?1 AND t.state = 'blocked' AND t.reason LIKE 'waits on task%'",
            )?;
            stmt.query_map(params![dep], |r| Ok((r.get("id")?, r.get("after_json")?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        release_or_reblock(&c, candidates)
    }

    /// Re-evaluate every currently blocked task whose reason says it
    /// waits on a dependency: the backstop run on each claim loop, so a
    /// dependent whose after list was re-pointed by a direct store edit —
    /// which fires no trigger of its own — still catches up once its
    /// (possibly new) dependency resolves. Returns the ids released to
    /// `queued`.
    pub fn release_dependents(&self) -> Result<Vec<i64>> {
        let c = self.lock();
        let candidates: Vec<(i64, String)> = {
            let mut stmt = c.prepare(
                "SELECT id, after_json FROM tasks WHERE state = 'blocked' AND reason LIKE 'waits on task%'",
            )?;
            stmt.query_map([], |r| Ok((r.get("id")?, r.get("after_json")?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        release_or_reblock(&c, candidates)
    }

    /// The first task in `id`'s chain of retries: itself when it retries nothing.
    pub fn root_of(&self, id: i64) -> Result<i64> {
        // Retries always point at an already-existing task, so ids only
        // shrink walking up the chain: the root is the smallest one.
        Ok(lineage_ids(&self.lock(), id)?.into_iter().min().unwrap())
    }

    /// Every task in `id`'s lineage, root first: the root and everything
    /// that retries it, directly or through other retries.
    pub fn lineage(&self, id: i64) -> Result<Vec<LineageRow>> {
        let root = self.root_of(id)?;
        let c = self.lock();
        let mut stmt = c.prepare(
            "WITH RECURSIVE down(id) AS (
               SELECT ?1 UNION ALL SELECT t.id FROM down JOIN tasks t ON t.retry_of = down.id)
             SELECT t.id, t.retry_of, t.state, t.reason, t.workflow,
                    COALESCE((SELECT SUM(cost_usd) FROM attempts a WHERE a.task_id = t.id), 0) AS cost
             FROM down JOIN tasks t ON t.id = down.id ORDER BY t.id",
        )?;
        let rows = stmt.query_map(params![root], |r| {
            Ok(LineageRow {
                id: r.get("id")?,
                parent: r.get("retry_of")?,
                state: r.get("state")?,
                reason: r.get("reason")?,
                workflow: r.get("workflow")?,
                cost: r.get("cost")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every task that retries `id` directly.
    pub fn dependents_retries(&self, id: i64) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT id FROM tasks WHERE retry_of=?1 ORDER BY id")?;
        let rows = stmt.query_map(params![id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The newest task that retries `id`, if any.
    pub fn latest_retry_of(&self, id: i64) -> Result<Option<i64>> {
        Ok(self.lock().query_row(
            "SELECT MAX(id) FROM tasks WHERE retry_of=?1",
            params![id],
            |r| r.get::<_, Option<i64>>(0),
        )?)
    }

    pub fn queued_count(&self) -> Result<i64> {
        Ok(self
            .lock()
            .query_row("SELECT COUNT(*) FROM tasks WHERE state='queued'", [], |r| {
                r.get(0)
            })?)
    }

    /// Put a running task back in the queue, closing its open attempt as
    /// agent_failed with `why`, so the next worker resumes at the following
    /// attempt number.
    pub fn requeue(&self, id: i64, why: &str) -> Result<()> {
        let c = self.lock();
        c.execute(
            "UPDATE attempts SET state='agent_failed', reason=?2, finished_at=?3 WHERE task_id=?1 AND state='running'",
            params![id, why, crate::unix_now()],
        )?;
        c.execute(
            "UPDATE tasks SET state='queued', worker_pid=NULL, reason=?2 WHERE id=?1 AND state='running'",
            params![id, format!("requeued: {why}")],
        )?;
        Ok(())
    }

    /// Tasks left in `running` by a worker that no longer exists.
    pub fn orphans(&self, alive: impl Fn(i64) -> bool) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT id, worker_pid FROM tasks WHERE state='running'")?;
        let running: Vec<(i64, Option<i64>)> = stmt
            .query_map([], |r| Ok((r.get("id")?, r.get("worker_pid")?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(running
            .into_iter()
            .filter(|(_, pid)| !pid.is_some_and(&alive))
            .map(|(id, _)| id)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_is_exclusive_and_requeue_closes_the_open_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let t = Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 2,
            timeout_secs: 1,
            ..Default::default()
        };
        let id = s.insert_task(&t).unwrap();
        assert!(s.claim(id, 1).unwrap());
        assert!(!s.claim(id, 2).unwrap(), "second claim must fail");
        let a = Attempt {
            task_id: id,
            attempt_no: 1,
            started_at: 0,
            ..Default::default()
        };
        s.insert_attempt(&a).unwrap();
        s.requeue(id, "worker died").unwrap();
        let t = s.task(id).unwrap().unwrap();
        assert_eq!(t.state, TaskState::Queued);
        let att = s.attempts(id).unwrap();
        assert_eq!(att[0].state, AttemptState::AgentFailed);
        assert_eq!(att[0].reason, "worker died");
        assert_eq!(
            s.claim_next(3, &[], |_| false).unwrap().map(|t| t.id),
            Some(id)
        );
    }

    #[test]
    fn update_task_persists_every_field() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let mut t = Task {
            repo: "/r".into(),
            task: "do".into(),
            base_branch: "main".into(),
            model: "sonnet".into(),
            max_turns: 30,
            max_attempts: 2,
            timeout_secs: 60,
            state: TaskState::Queued,
            created_at: 1,
            workflow: "direct".into(),
            land: true,
            journal: true,
            context_enabled: true,
            ..Default::default()
        };
        t.id = store.insert_task(&t).unwrap();
        // Change every mutable field, then read it back.
        t.repo = "/elsewhere".into();
        t.task = "do more".into();
        t.base_branch = "dev".into();
        t.base_sha = "abc".into();
        t.branch = "forge/x".into();
        t.worktree = "/wt".into();
        t.model = "opus".into();
        t.max_turns = 99;
        t.max_attempts = 5;
        t.timeout_secs = 7;
        t.checks = vec!["true".into()];
        t.state = TaskState::Running;
        t.reason = "why".into();
        t.question_to = Some("alice".into());
        t.started_at = Some(2);
        t.finished_at = Some(3);
        t.pushed = true;
        t.worker_pid = Some(4);
        t.budget_usd = Some(1.5);
        t.allow_protected = true;
        t.workflow = "tdd".into();
        t.workflow_hash = "h".into();
        t.workflow_text = "text".into();
        t.actions_json = "[]".into();
        t.interface = "iface".into();
        t.show_checks = true;
        t.land = false;
        t.after = vec![7, 8];
        t.verify_base = "vb".into();
        t.retry_of = Some(9);
        t.journal = false;
        t.context = "ctx".into();
        t.context_enabled = false;
        t.resume_on_failure = true;
        t.plan = "plan".into();
        t.landed_sha = "abc123".into();
        t.journal_arm = "control".into();
        t.project = Some("proj".into());
        t.initiative = Some(11);
        t.landed_at = Some(4);
        t.hand_landed = true;
        store.update_task(&t).unwrap();
        let back = store.task(t.id).unwrap().unwrap();
        assert_eq!(format!("{back:?}"), format!("{t:?}"));
    }
}
