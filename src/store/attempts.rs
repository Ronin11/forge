use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AttemptState {
    #[default]
    Running,
    Succeeded,
    ChecksFailed,
    AgentFailed,
    Unverified,
    /// The agent asked the operator a question; retrying cannot answer it.
    NeedsInput,
}

impl AttemptState {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptState::Running => "running",
            AttemptState::Succeeded => "succeeded",
            AttemptState::ChecksFailed => "checks_failed",
            AttemptState::AgentFailed => "agent_failed",
            AttemptState::Unverified => "unverified",
            AttemptState::NeedsInput => "needs_input",
        }
    }
}

impl TryFrom<&str> for AttemptState {
    type Error = std::io::Error;
    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        Ok(match s {
            "running" => AttemptState::Running,
            "succeeded" => AttemptState::Succeeded,
            "checks_failed" => AttemptState::ChecksFailed,
            "agent_failed" => AttemptState::AgentFailed,
            "unverified" => AttemptState::Unverified,
            "needs_input" => AttemptState::NeedsInput,
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown attempt state {other:?}"
                )));
            }
        })
    }
}

/// What `Store::reprice_attempts` did: how many attempts it repriced and
/// their combined new cost.
#[derive(Default, Debug, Clone, Copy)]
pub struct RepriceResult {
    pub changed: i64,
    pub total_usd: f64,
}

#[derive(Default, Debug, Clone)]
pub struct Attempt {
    pub id: i64,
    pub task_id: i64,
    pub attempt_no: i64,
    pub step: String,
    /// Index of the step in the resolved workflow, for resumption.
    pub step_seq: i64,
    /// HEAD when the attempt started: "what you changed" means since here.
    pub start_sha: String,
    pub end_sha: String,
    /// The runner and provider this attempt ran under (see
    /// `agent::Runner`/`agent::Provider`); the model is on `inputs_json`.
    pub runner: String,
    pub provider: String,
    /// audit::Inputs as JSON: everything the step was given.
    pub inputs_json: String,
    /// audit::Outputs as JSON: everything the step produced beyond the verdict.
    pub outputs_json: String,
    pub state: AttemptState,
    pub reason: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub agent_exit: Option<i32>,
    pub timed_out: bool,
    pub num_turns: i64,
    pub tool_calls: i64,
    pub cost_usd: Option<f64>,
    pub agent_ms: i64,
    pub commits: i64,
    pub files_changed: i64,
    pub dirty: bool,
    pub verdict_json: String,
    pub result_text: String,
    pub log_path: String,
    /// The structured result as the CLI produced it, raw JSON; empty if none.
    pub envelope_json: String,
    pub rl_five_hour: Option<f64>,
    pub rl_seven_day: Option<f64>,
    pub rl_five_hour_resets: Option<i64>,
    pub rl_seven_day_resets: Option<i64>,
    /// The CLI session the attempt ran in; empty when the stream never said.
    pub session_id: String,
    /// Tool calls before the first edit; `None` when it never edited.
    pub first_edit: Option<i64>,
    /// Token counts from the result frame's usage object.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    /// `agent::Outcome::early_signals` as JSON: which of `Watch`'s signs
    /// tripped, whether or not they ended the run.
    pub early_signals: String,
    /// `agent::Outcome::early_near` as JSON: which signs were within 20%
    /// of tripping and did not, so the thresholds can be tuned from here.
    pub early_near: String,
}

/// Everything `Store::finish_attempt` writes back for an attempt that has run to completion.
#[derive(Default)]
pub struct FinishAttempt {
    /// The attempt row to update.
    pub id: i64,
    pub state: AttemptState,
    pub reason: String,
    pub finished_at: Option<i64>,
    pub agent_exit: Option<i32>,
    pub timed_out: bool,
    pub num_turns: i64,
    pub tool_calls: i64,
    pub cost_usd: Option<f64>,
    pub agent_ms: i64,
    pub commits: i64,
    pub files_changed: i64,
    pub dirty: bool,
    pub verdict_json: String,
    pub result_text: String,
    /// The structured result as the CLI produced it, raw JSON; empty if none.
    pub envelope_json: String,
    pub rl_five_hour: Option<f64>,
    pub rl_seven_day: Option<f64>,
    /// Unix seconds at which each window resets, as the CLI reported.
    pub rl_five_hour_resets: Option<i64>,
    pub rl_seven_day_resets: Option<i64>,
    /// HEAD when the attempt finished.
    pub end_sha: String,
    /// audit::Outputs as JSON: everything the step produced beyond the verdict.
    pub outputs_json: String,
    /// The CLI session the attempt ran in; empty when the stream never said.
    pub session_id: String,
    /// Tool calls before the first edit; `None` when it never edited.
    pub first_edit: Option<i64>,
    /// Token counts from the result frame's usage object.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub early_signals: String,
    pub early_near: String,
}

pub struct RateLimitSample {
    pub seen_at: i64,
    pub five_hour: Option<f64>,
    pub seven_day: Option<f64>,
    /// Unix seconds at which each window resets, as the CLI reported.
    pub five_hour_resets: Option<i64>,
    pub seven_day_resets: Option<i64>,
}

/// One operation, kernel or user, as it ran.
#[derive(Default, Debug, Clone)]
pub struct Op {
    pub id: i64,
    pub task_id: i64,
    pub seq: i64,
    pub name: String,
    pub kernel: bool,
    pub started_at: i64,
    pub ms: i64,
    pub ok: bool,
    pub exit: Option<i32>,
    pub detail: String,
    pub attempt_id: Option<i64>,
    /// What the operation produced, when it produces a value: its stdout.
    pub output: String,
}

/// Every column of the `attempts` table (`store::column_tests::
/// the_column_lists_agree_with_the_schema` enforces the two agree).
/// `repriced_at` has no field on `Attempt`: `Store::reprice_attempts` is
/// the only reader, and it queries the column directly rather than going
/// through this struct.
pub(super) const ATTEMPT_COLUMNS: &[&str] = &[
    "id",
    "task_id",
    "attempt_no",
    "state",
    "reason",
    "started_at",
    "finished_at",
    "agent_exit",
    "timed_out",
    "num_turns",
    "tool_calls",
    "cost_usd",
    "agent_ms",
    "commits",
    "files_changed",
    "dirty",
    "verdict_json",
    "result_text",
    "log_path",
    "envelope_json",
    "rl_five_hour",
    "rl_seven_day",
    "rl_five_hour_resets",
    "rl_seven_day_resets",
    "step",
    "start_sha",
    "end_sha",
    "inputs_json",
    "outputs_json",
    "step_seq",
    "session_id",
    "first_edit",
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
    "early_signals",
    "early_near",
    "runner",
    "provider",
    "repriced_at",
];

pub(super) const OP_COLUMNS: &[&str] = &[
    "id",
    "task_id",
    "seq",
    "name",
    "kernel",
    "started_at",
    "ms",
    "ok",
    "exit",
    "detail",
    "attempt_id",
    "output",
];

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

    /// (step, outputs_json) of the last `n` attempts, newest first.
    pub fn recent_attempt_outputs(&self, n: i64) -> Result<Vec<(String, String)>> {
        let c = self.lock();
        let mut stmt =
            c.prepare("SELECT step, outputs_json FROM attempts ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![n], |r| Ok((r.get("step")?, r.get("outputs_json")?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// `forge stats --reprice` (docs/ECONOMIST.md, "Repricing a
    /// free-reporting provider"): for every attempt with `cost_usd` 0 or
    /// NULL, recorded `input_tokens`/`output_tokens`, and a provider
    /// `prices` names (provider -> (price per million input tokens, price
    /// per million output tokens), the operator config's own numbers),
    /// sets `cost_usd` to tokens times price — the same arithmetic
    /// `agent.rs` uses when a provider reports it live. `provider`
    /// narrows to one provider's attempts. Without `force`, only a row
    /// that has never been repriced (`repriced_at` NULL) and still reads
    /// `cost_usd` 0 or NULL is touched; a row whose provider reported a
    /// real cost at launch is never selected, repriced or not. With
    /// `force`, every row this verb repriced before (`repriced_at` not
    /// NULL) is redone from its tokens and the current price, whatever
    /// its `cost_usd` reads now — a repriced row's `cost_usd` no longer
    /// gates whether `--force` can reach it.
    pub fn reprice_attempts(
        &self,
        provider: Option<&str>,
        force: bool,
        prices: &BTreeMap<String, (f64, f64)>,
    ) -> Result<RepriceResult> {
        let c = self.lock();
        let candidates: Vec<(i64, String, i64, i64)> = {
            let mut stmt = c.prepare(
                "SELECT id, provider, input_tokens, output_tokens FROM attempts
                 WHERE input_tokens IS NOT NULL AND output_tokens IS NOT NULL
                   AND (?1 IS NULL OR provider = ?1)
                   AND (((cost_usd = 0 OR cost_usd IS NULL) AND repriced_at IS NULL)
                        OR (?2 = 1 AND repriced_at IS NOT NULL))",
            )?;
            stmt.query_map(params![provider, force as i64], |r| {
                Ok((
                    r.get("id")?,
                    r.get("provider")?,
                    r.get("input_tokens")?,
                    r.get("output_tokens")?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let now = crate::unix_now();
        let mut result = RepriceResult::default();
        for (id, provider, input_tokens, output_tokens) in candidates {
            let Some((price_input, price_output)) = prices.get(&provider) else {
                continue;
            };
            let cost = input_tokens as f64 * price_input / 1_000_000.0
                + output_tokens as f64 * price_output / 1_000_000.0;
            c.execute(
                "UPDATE attempts SET cost_usd=?2, repriced_at=?3 WHERE id=?1",
                params![id, cost, now],
            )?;
            result.changed += 1;
            result.total_usd += cost;
        }
        Ok(result)
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

    pub fn mark_worktree_removed(&self, id: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE tasks SET worktree_removed_at=?2 WHERE id=?1",
            params![id, crate::unix_now()],
        )?;
        Ok(())
    }
}

impl Attempt {
    /// An attempt an agent made, as opposed to a row the kernel wrote
    /// about the task: the supervisor's rulings and the integrator's
    /// check runs are on the record but are not the agent's work, so a
    /// rule about "the last attempt" skips them.
    pub fn is_agent(&self) -> bool {
        self.step != "supervisor" && self.step != "integrate"
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

    /// Fixture for the reprice tests: one task, and a helper to insert an
    /// attempt under it with a given provider, cost, and token counts.
    fn reprice_fixture() -> (tempfile::TempDir, Store, i64) {
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
        let task_id = s.insert_task(&t).unwrap();
        (dir, s, task_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn reprice_attempt(
        s: &Store,
        task_id: i64,
        attempt_no: i64,
        provider: &str,
        cost_usd: Option<f64>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    ) -> i64 {
        let id = s
            .insert_attempt(&Attempt {
                task_id,
                attempt_no,
                started_at: attempt_no,
                provider: provider.into(),
                ..Default::default()
            })
            .unwrap();
        s.finish_attempt(&FinishAttempt {
            id,
            state: AttemptState::Succeeded,
            reason: String::new(),
            finished_at: Some(attempt_no),
            agent_exit: Some(0),
            timed_out: false,
            num_turns: 1,
            tool_calls: 1,
            cost_usd,
            agent_ms: 0,
            commits: 0,
            files_changed: 0,
            dirty: false,
            verdict_json: "[]".into(),
            result_text: String::new(),
            envelope_json: String::new(),
            rl_five_hour: None,
            rl_seven_day: None,
            rl_five_hour_resets: None,
            rl_seven_day_resets: None,
            end_sha: String::new(),
            outputs_json: String::new(),
            session_id: String::new(),
            first_edit: None,
            input_tokens,
            output_tokens,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            early_signals: "[]".into(),
            early_near: "[]".into(),
        })
        .unwrap();
        id
    }

    #[test]
    fn reprice_attempts_prices_tokens_for_zero_cost_rows_with_a_priced_provider() {
        let (_dir, s, task_id) = reprice_fixture();
        // codex-style: cost_usd 0, tokens recorded.
        let zero_cost = reprice_attempt(
            &s,
            task_id,
            1,
            "openai",
            Some(0.0),
            Some(1_000_000),
            Some(500_000),
        );
        // never got a cost at all.
        let null_cost =
            reprice_attempt(&s, task_id, 2, "openai", None, Some(200_000), Some(100_000));
        // a real, nonzero reported cost: never touched.
        let real_cost = reprice_attempt(
            &s,
            task_id,
            3,
            "anthropic",
            Some(1.23),
            Some(1_000_000),
            Some(1_000_000),
        );
        // no price configured for this provider: left as it was.
        let unpriced = reprice_attempt(
            &s,
            task_id,
            4,
            "devhome",
            Some(0.0),
            Some(1_000_000),
            Some(1_000_000),
        );
        // no recorded tokens: nothing to price it from.
        let no_tokens = reprice_attempt(&s, task_id, 5, "openai", Some(0.0), None, None);

        let prices = BTreeMap::from([
            ("openai".to_string(), (2.0, 6.0)),
            ("anthropic".to_string(), (3.0, 15.0)),
        ]);
        let result = s.reprice_attempts(None, false, &prices).unwrap();
        assert_eq!(result.changed, 2);
        assert!(
            (result.total_usd - 6.0).abs() < 1e-9,
            "{}",
            result.total_usd
        );

        let cost = |id: i64| {
            s.attempts(task_id)
                .unwrap()
                .into_iter()
                .find(|a| a.id == id)
                .unwrap()
                .cost_usd
        };
        assert!((cost(zero_cost).unwrap() - 5.0).abs() < 1e-9);
        assert!((cost(null_cost).unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(cost(real_cost), Some(1.23));
        assert_eq!(cost(unpriced), Some(0.0));
        assert_eq!(cost(no_tokens), Some(0.0));
    }

    #[test]
    fn reprice_attempts_narrows_to_the_named_provider() {
        let (_dir, s, task_id) = reprice_fixture();
        reprice_attempt(
            &s,
            task_id,
            1,
            "openai",
            Some(0.0),
            Some(1_000_000),
            Some(0),
        );
        reprice_attempt(
            &s,
            task_id,
            2,
            "anthropic",
            Some(0.0),
            Some(1_000_000),
            Some(0),
        );
        let prices = BTreeMap::from([
            ("openai".to_string(), (1.0, 1.0)),
            ("anthropic".to_string(), (1.0, 1.0)),
        ]);
        let result = s.reprice_attempts(Some("openai"), false, &prices).unwrap();
        assert_eq!(result.changed, 1);
        assert!((result.total_usd - 1.0).abs() < 1e-9);
    }

    #[test]
    fn reprice_attempts_is_idempotent_unless_forced() {
        let (_dir, s, task_id) = reprice_fixture();
        // priced at exactly zero, so the row's cost_usd stays 0 after
        // repricing too — repriced_at, not the cost value, is what a
        // rerun must check to skip it.
        reprice_attempt(
            &s,
            task_id,
            1,
            "openai",
            Some(0.0),
            Some(1_000_000),
            Some(0),
        );
        let prices = BTreeMap::from([("openai".to_string(), (0.0, 0.0))]);

        let first = s.reprice_attempts(None, false, &prices).unwrap();
        assert_eq!(first.changed, 1);

        let second = s.reprice_attempts(None, false, &prices).unwrap();
        assert_eq!(second.changed, 0, "already repriced; a rerun is a no-op");

        let forced = s.reprice_attempts(None, true, &prices).unwrap();
        assert_eq!(forced.changed, 1, "--force redoes an already-repriced row");
    }

    #[test]
    fn reprice_attempts_force_redoes_a_row_priced_nonzero_but_never_touches_a_reported_cost() {
        let (_dir, s, task_id) = reprice_fixture();
        // repriced once, lands on a real nonzero cost — the normal case.
        let repriced = reprice_attempt(
            &s,
            task_id,
            1,
            "openai",
            Some(0.0),
            Some(1_000_000),
            Some(500_000),
        );
        // a real cost the provider itself reported at launch: never repriced,
        // forced or not.
        let reported = reprice_attempt(
            &s,
            task_id,
            2,
            "openai",
            Some(1.23),
            Some(1_000_000),
            Some(500_000),
        );
        let prices = BTreeMap::from([("openai".to_string(), (2.0, 6.0))]);

        let cost = |id: i64| {
            s.attempts(task_id)
                .unwrap()
                .into_iter()
                .find(|a| a.id == id)
                .unwrap()
                .cost_usd
        };

        let first = s.reprice_attempts(None, false, &prices).unwrap();
        assert_eq!(first.changed, 1);
        assert!((first.total_usd - 5.0).abs() < 1e-9, "{}", first.total_usd);
        assert!((cost(repriced).unwrap() - 5.0).abs() < 1e-9);
        assert_eq!(cost(reported), Some(1.23));

        let second = s.reprice_attempts(None, false, &prices).unwrap();
        assert_eq!(second.changed, 0, "already repriced; a rerun is a no-op");

        let forced = s.reprice_attempts(None, true, &prices).unwrap();
        assert_eq!(
            forced.changed, 1,
            "--force redoes the row despite its nonzero cost_usd"
        );
        assert!(
            (forced.total_usd - 5.0).abs() < 1e-9,
            "{}",
            forced.total_usd
        );
        assert!((cost(repriced).unwrap() - 5.0).abs() < 1e-9);
        assert_eq!(
            cost(reported),
            Some(1.23),
            "a reported cost is never overwritten"
        );
    }
}
