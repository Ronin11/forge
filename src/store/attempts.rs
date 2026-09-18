use super::*;

fn attempt_from_row(r: &Row) -> rusqlite::Result<Attempt> {
    Ok(Attempt {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        attempt_no: r.get("attempt_no")?,
        state: conv(
            r,
            "state",
            AttemptState::try_from(r.get::<_, String>("state")?.as_str()),
        )?,
        reason: r.get("reason")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        agent_exit: r.get("agent_exit")?,
        timed_out: r.get::<_, i64>("timed_out")? != 0,
        num_turns: r.get("num_turns")?,
        tool_calls: r.get("tool_calls")?,
        cost_usd: r.get("cost_usd")?,
        agent_ms: r.get("agent_ms")?,
        commits: r.get("commits")?,
        files_changed: r.get("files_changed")?,
        dirty: r.get::<_, i64>("dirty")? != 0,
        verdict_json: r.get("verdict_json")?,
        result_text: r.get("result_text")?,
        log_path: r.get("log_path")?,
        envelope_json: r.get("envelope_json")?,
        rl_five_hour: r.get("rl_five_hour")?,
        rl_seven_day: r.get("rl_seven_day")?,
        rl_five_hour_resets: r.get("rl_five_hour_resets")?,
        rl_seven_day_resets: r.get("rl_seven_day_resets")?,
        step: r.get("step")?,
        start_sha: r.get("start_sha")?,
        end_sha: r.get("end_sha")?,
        inputs_json: r.get("inputs_json")?,
        outputs_json: r.get("outputs_json")?,
        step_seq: r.get("step_seq")?,
        session_id: r.get("session_id")?,
        first_edit: r.get("first_edit")?,
        input_tokens: r.get("input_tokens")?,
        output_tokens: r.get("output_tokens")?,
        cache_read_input_tokens: r.get("cache_read_input_tokens")?,
        cache_creation_input_tokens: r.get("cache_creation_input_tokens")?,
        early_signals: r.get("early_signals")?,
        early_near: r.get("early_near")?,
        runner: r.get("runner")?,
        provider: r.get("provider")?,
    })
}

fn op_from_row(r: &Row) -> rusqlite::Result<Op> {
    Ok(Op {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        seq: r.get("seq")?,
        name: r.get("name")?,
        kernel: r.get::<_, i64>("kernel")? != 0,
        started_at: r.get("started_at")?,
        ms: r.get("ms")?,
        ok: r.get::<_, i64>("ok")? != 0,
        exit: r.get("exit")?,
        detail: r.get("detail")?,
        attempt_id: r.get("attempt_id")?,
        output: r.get("output")?,
    })
}

impl Store {
    pub fn insert_attempt(&self, a: &Attempt) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO attempts (task_id, attempt_no, state, started_at, log_path, step, start_sha, inputs_json, step_seq, runner, provider)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![a.task_id, a.attempt_no, a.state.as_str(), a.started_at, a.log_path, a.step, a.start_sha, a.inputs_json, a.step_seq, a.runner, a.provider],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn finish_attempt(&self, a: &FinishAttempt) -> Result<()> {
        self.lock().execute(
            "UPDATE attempts SET state=?2, reason=?3, finished_at=?4, agent_exit=?5, timed_out=?6, num_turns=?7,
             tool_calls=?8, cost_usd=?9, agent_ms=?10, commits=?11, files_changed=?12, dirty=?13, verdict_json=?14,
             result_text=?15, envelope_json=?16, rl_five_hour=?17, rl_seven_day=?18, rl_five_hour_resets=?19,
             rl_seven_day_resets=?20, end_sha=?21, outputs_json=?22, session_id=?23, first_edit=?24,
             input_tokens=?25, output_tokens=?26, cache_read_input_tokens=?27, cache_creation_input_tokens=?28,
             early_signals=?29, early_near=?30 WHERE id=?1",
            params![
                a.id,
                a.state.as_str(),
                a.reason,
                a.finished_at,
                a.agent_exit,
                a.timed_out as i64,
                a.num_turns,
                a.tool_calls,
                a.cost_usd,
                a.agent_ms,
                a.commits,
                a.files_changed,
                a.dirty as i64,
                a.verdict_json,
                a.result_text,
                a.envelope_json,
                a.rl_five_hour,
                a.rl_seven_day,
                a.rl_five_hour_resets,
                a.rl_seven_day_resets,
                a.end_sha,
                a.outputs_json,
                a.session_id,
                a.first_edit,
                a.input_tokens,
                a.output_tokens,
                a.cache_read_input_tokens,
                a.cache_creation_input_tokens,
                a.early_signals,
                a.early_near,
            ],
        )?;
        Ok(())
    }

    pub fn running_ids(&self) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT id FROM tasks WHERE state='running' ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The most recent rate-limit sample any attempt on `provider` recorded:
    /// the window hold is per provider, since each has its own subscription
    /// (or none at all).
    pub fn latest_rate_limit(&self, provider: &str) -> Result<Option<RateLimitSample>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT COALESCE(finished_at, started_at) AS seen_at, rl_five_hour, rl_seven_day, rl_five_hour_resets, rl_seven_day_resets FROM attempts
                 WHERE provider = ?1 AND (rl_five_hour IS NOT NULL OR rl_seven_day IS NOT NULL) ORDER BY id DESC LIMIT 1",
                params![provider],
                |r| {
                    Ok(RateLimitSample {
                        seen_at: r.get("seen_at")?,
                        five_hour: r.get("rl_five_hour")?,
                        seven_day: r.get("rl_seven_day")?,
                        five_hour_resets: r.get("rl_five_hour_resets")?,
                        seven_day_resets: r.get("rl_seven_day_resets")?,
                    })
                },
            )
            .optional()?)
    }

    pub fn insert_op(&self, o: &Op) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO ops (task_id, seq, name, kernel, started_at, ms, ok, exit, detail, attempt_id, output)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![o.task_id, o.seq, o.name, o.kernel as i64, o.started_at, o.ms, o.ok as i64, o.exit, o.detail, o.attempt_id, o.output],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn ops(&self, task_id: i64) -> Result<Vec<Op>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM ops WHERE task_id=?1 ORDER BY id",
            OP_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![task_id], op_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn attempts(&self, task_id: i64) -> Result<Vec<Attempt>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM attempts WHERE task_id=?1 ORDER BY attempt_no",
            ATTEMPT_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![task_id], attempt_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// (task_id, step, outputs_json) for every attempt, in one query, so
    /// callers can pull tool facts out of outputs_json without an N+1 over
    /// tasks. Optionally restricted to a single step.
    pub fn attempt_tool_facts(&self, step: Option<&str>) -> Result<Vec<(i64, String, String)>> {
        let c = self.lock();
        let rows = match step {
            Some(step) => {
                let mut stmt =
                    c.prepare("SELECT task_id, step, outputs_json FROM attempts WHERE step=?1")?;
                let rows = stmt.query_map(params![step], |r| {
                    Ok((r.get("task_id")?, r.get("step")?, r.get("outputs_json")?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut stmt = c.prepare("SELECT task_id, step, outputs_json FROM attempts")?;
                let rows = stmt.query_map([], |r| {
                    Ok((r.get("task_id")?, r.get("step")?, r.get("outputs_json")?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    /// Total cost of a task's attempts so far, from the CLI's accounting.
    pub fn task_cost(&self, task_id: i64) -> Result<f64> {
        Ok(self.lock().query_row(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM attempts WHERE task_id=?1",
            params![task_id],
            |r| r.get(0),
        )?)
    }

    /// Cost of every attempt started at or after `since`.
    /// The files successful attempts on this repository read most: a prior
    /// for where a new task's answer is likely to be. From the tool facts.
    pub fn hot_files(&self, repo: &str, n: usize) -> Result<Vec<String>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT a.outputs_json FROM attempts a JOIN tasks t ON t.id = a.task_id
             WHERE t.repo = ?1 AND a.state = 'succeeded' AND a.step != 'review'",
        )?;
        let rows: Vec<String> = stmt
            .query_map(params![repo], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for json in rows {
            if let Ok(o) = serde_json::from_str::<crate::audit::Outputs>(&json)
                && let Some(t) = o.tools
            {
                for (path, k) in t.reads {
                    if !path.starts_with('/') {
                        *counts.entry(path).or_default() += k;
                    }
                }
            }
        }
        let mut v: Vec<(String, u64)> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(v.into_iter().take(n).map(|(p, _)| p).collect())
    }

    pub fn spent_since(&self, since: i64) -> Result<f64> {
        Ok(self.lock().query_row(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM attempts WHERE started_at >= ?1",
            params![since],
            |r| r.get(0),
        )?)
    }

    /// Tasks whose worktree is still on disk as far as Forge knows.
    pub fn tasks_with_worktrees(&self) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks WHERE worktree != '' AND worktree_removed_at IS NULL ORDER BY id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map([], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Landed tasks, scoped like every other `forge stats` query: the
    /// input to both delayed-cost signals (the `task_repair_cost` cache
    /// and the `task_churn` cache).
    pub fn landed_tasks(&self, scope: &StatsFilter) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks WHERE landed_sha != ''
               AND (?1 IS NULL OR project = ?1) AND (?2 IS NULL OR initiative = ?2)
             ORDER BY id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![scope.project, scope.initiative], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Landed tasks on `repo`, other than `exclude`, whose landing fell in
    /// `(from, to]`: the later landings a churn computation diffs `added`
    /// against (see docs/LATER.md, the delayed-cost follow-up to "Defect
    /// escape").
    pub fn later_landings(
        &self,
        repo: &str,
        exclude: i64,
        from: i64,
        to: i64,
    ) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks WHERE repo = ?1 AND id != ?2 AND landed_sha != ''
               AND finished_at > ?3 AND finished_at <= ?4
             ORDER BY id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![repo, exclude, from, to], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The most recent landed task on `repo` with an id below `before_id`:
    /// what `refresh_hand_commits` diffs a landing against to find the
    /// hand commits that reached the base branch since (see
    /// `Task::landed_at`, `task_hand_commits`). `None` for the first
    /// landing a repository ever gets.
    pub fn previous_landing(&self, repo: &str, before_id: i64) -> Result<Option<Task>> {
        let c = self.lock();
        Ok(c.query_row(
            &format!(
                "SELECT {} FROM tasks WHERE repo = ?1 AND id < ?2 AND landed_sha != ''
                 ORDER BY id DESC LIMIT 1",
                TASK_COLUMNS.join(", ")
            ),
            params![repo, before_id],
            task_from_row,
        )
        .optional()?)
    }

    pub fn mark_worktree_removed(&self, id: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE tasks SET worktree_removed_at=?2 WHERE id=?1",
            params![id, crate::unix_now()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_rate_limit_is_keyed_by_provider() {
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
        let finish = |aid: i64, five_hour: f64| FinishAttempt {
            id: aid,
            state: AttemptState::Succeeded,
            reason: String::new(),
            finished_at: Some(2),
            agent_exit: Some(0),
            timed_out: false,
            num_turns: 1,
            tool_calls: 1,
            cost_usd: Some(0.0),
            agent_ms: 0,
            commits: 0,
            files_changed: 0,
            dirty: false,
            verdict_json: "[]".into(),
            result_text: String::new(),
            envelope_json: String::new(),
            rl_five_hour: Some(five_hour),
            rl_seven_day: None,
            rl_five_hour_resets: Some(2_000_000_000),
            rl_seven_day_resets: None,
            end_sha: String::new(),
            outputs_json: String::new(),
            session_id: String::new(),
            first_edit: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            early_signals: "[]".into(),
            early_near: "[]".into(),
        };
        let anthropic_attempt = s
            .insert_attempt(&Attempt {
                task_id: id,
                attempt_no: 1,
                started_at: 1,
                provider: "anthropic".into(),
                ..Default::default()
            })
            .unwrap();
        s.finish_attempt(&finish(anthropic_attempt, 0.95)).unwrap();
        let devhome_attempt = s
            .insert_attempt(&Attempt {
                task_id: id,
                attempt_no: 2,
                started_at: 2,
                provider: "devhome".into(),
                ..Default::default()
            })
            .unwrap();
        s.finish_attempt(&finish(devhome_attempt, 0.1)).unwrap();
        assert_eq!(
            s.latest_rate_limit("anthropic").unwrap().unwrap().five_hour,
            Some(0.95)
        );
        assert_eq!(
            s.latest_rate_limit("devhome").unwrap().unwrap().five_hour,
            Some(0.1)
        );
        assert!(s.latest_rate_limit("openai").unwrap().is_none());
    }
}
