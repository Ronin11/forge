use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TaskState {
    #[default]
    Queued,
    Running,
    Succeeded,
    Failed,
    Unverified,
    /// The agent asked a question or for a different workflow; not a failure.
    Blocked,
    /// The operator decided this should not be done: a stale description,
    /// superseded, or the product decision went the other way. Terminal,
    /// like `Failed`, but never a defect in the work.
    Withdrawn,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Running => "running",
            TaskState::Succeeded => "succeeded",
            TaskState::Failed => "failed",
            TaskState::Unverified => "unverified",
            TaskState::Blocked => "blocked",
            TaskState::Withdrawn => "withdrawn",
        }
    }
}

impl TryFrom<&str> for TaskState {
    type Error = std::io::Error;
    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        Ok(match s {
            "queued" => TaskState::Queued,
            "running" => TaskState::Running,
            "succeeded" => TaskState::Succeeded,
            "failed" => TaskState::Failed,
            "unverified" => TaskState::Unverified,
            "blocked" => TaskState::Blocked,
            "withdrawn" => TaskState::Withdrawn,
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown task state {other:?}"
                )));
            }
        })
    }
}

#[derive(Default, Debug, Clone)]
pub struct Task {
    pub id: i64,
    pub repo: String,
    pub task: String,
    /// The task in the customer's own words, for the day it was filed
    /// that way (`forge add --title`, or the concierge on a filed
    /// request): what `PortalDoc`'s "Done" line uses instead of deriving
    /// one from `task` (see docs/PORTAL.md). `None` for every task filed
    /// before this column, or never given one.
    pub title: Option<String>,
    pub base_branch: String,
    pub base_sha: String,
    pub branch: String,
    pub worktree: String,
    pub model: String,
    /// The provider name every agent step of this task runs under (see
    /// `agent::Provider`); the supervisor keeps its own model setting and
    /// is unaffected by this.
    pub provider: String,
    pub max_turns: i64,
    pub max_attempts: i64,
    pub timeout_secs: i64,
    /// Operator-declared acceptance commands, run as L2 after the repo's checks.
    pub checks: Vec<String>,
    pub state: TaskState,
    pub reason: String,
    /// Who a blocked question is addressed to (a channel contact's name,
    /// e.g. from the Signal plugin's `CONTACTS`); `None` means the
    /// operator. Set from the envelope's `needs_input.to` when the task
    /// blocks; meaningless outside `TaskState::Blocked`.
    pub question_to: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub pushed: bool,
    pub worker_pid: Option<i64>,
    /// Per-task cap override; `None` means the operator config's default.
    pub budget_usd: Option<f64>,
    pub worktree_removed_at: Option<i64>,
    /// The operator said this task may change protected paths.
    pub allow_protected: bool,
    pub workflow: String,
    /// Content hash of the workflow file the task ran under.
    pub workflow_hash: String,
    /// The workflow file's exact text at resolution, so the run is
    /// self-describing even after the file changes.
    pub workflow_text: String,
    /// workflows::Resolved as JSON: every action version the task runs,
    /// recorded at start; empty until then.
    pub actions_json: String,
    /// The tests step's summary: what the coder is told about the tests.
    pub interface: String,
    /// Show the L2 acceptance commands to the coder (default hidden).
    pub show_checks: bool,
    /// Land on the base branch once verified (the default); false leaves
    /// the verified branch pushed for a human to merge.
    pub land: bool,
    /// Tasks this one waits for: claimable only once every one of them has
    /// landed; blocked if any of them ends otherwise.
    pub after: Vec<i64>,
    /// The `forge-verify` commit that matches the base at clone time: the
    /// standing suite the task is judged by. Landing uses the current tip,
    /// since only the merged tree has everything the base gained since.
    pub verify_base: String,
    /// The task this one re-queues, when it was made by `forge retry`.
    pub retry_of: Option<i64>,
    /// Show the agents the journal of earlier attempts (the default);
    /// false for the control arm of a measurement.
    pub journal: bool,
    /// How `journal` got its value: "explicit" when the request said
    /// `--journal` or `--no-journal` itself, else "control" or "treatment"
    /// from the operator's `[measure] journal_control` fraction, drawn
    /// deterministically from the task id.
    pub journal_arm: String,
    /// What the last `context` operation printed: where things are.
    pub context: String,
    /// Show the agents that context (the default); false for the control arm.
    pub context_enabled: bool,
    /// After an attempt fails its checks, hand the next one --resume with
    /// the same CLI session instead of a fresh one. Preserved by `forge retry`.
    pub resume_on_failure: bool,
    /// What the last `plan` directive returned: the plan every later
    /// directive on this task is shown.
    pub plan: String,
    /// The base commit the task's branch became, once landed; empty until
    /// then. The scheduler's notion of "landed" is this column, not the
    /// wording of `reason`.
    pub landed_sha: String,
    /// When the task landed, `None` until then. Distinct from
    /// `finished_at`: a task verified before it had anywhere to land, or
    /// queued `--no-land`, sets `finished_at` at verification and only
    /// gets `landed_at` later, when a human's `forge land` (or the
    /// supervisor's own accept-and-land) actually lands it.
    pub landed_at: Option<i64>,
    /// Landed by a human's `forge land`, never by the supervisor's own
    /// automated landing: one of the human-attention signals (see
    /// `Store::human_attention_stats`).
    pub hand_landed: bool,
    /// The project this task belongs to; `None` for tasks predating
    /// projects that no migration could place, or whose repository lists
    /// more than one project.
    pub project: Option<String>,
    /// The initiative this task belongs to, if any.
    pub initiative: Option<i64>,
    /// Which provider each role drew from the operator's `[measure]
    /// explore` fractions, keyed by role name; empty when the task named
    /// an explicit `--provider` (which routes every role itself) or no
    /// role was configured to explore. Drawn once at creation, the same
    /// deterministic way as `journal_arm` (see `queue::assign_explore`),
    /// and consulted by `ctx::resolve_provider` at every step.
    pub explore: BTreeMap<String, String>,
    /// The concierge decision that produced this task, raw JSON, when
    /// `forge ask` filed it (a `request`, a `need`, or the placeholder for
    /// `unclear`); `None` for a task filed any other way (see
    /// docs/INTAKE.md, "The front door is not the interview").
    pub concierge_json: Option<String>,
    /// The escalator's proposal, raw JSON (`task_ids`, `repetition`,
    /// `outcome`), on the placeholder task `forge ask` blocks when the
    /// concierge's decision names a `pattern`; `None` for every other task
    /// (see docs/INTAKE.md, "The escalator").
    pub proposal_json: Option<String>,
    /// How the proposal was answered, "yes" or "no"; `None` while it is
    /// still blocked.
    pub proposal_answer: Option<String>,
    /// The initiative a "yes" answer filed; `None` for a "no" or a still-open proposal.
    pub proposal_initiative: Option<i64>,
    /// Task shape at intake (see docs/ECONOMIST.md, "Task shape"): what the
    /// economist must condition on before the task even runs, computed
    /// once at enqueue (`queue::task_shape`) and never revisited. The
    /// text's length in characters.
    pub shape_text_len: i64,
    /// How many of the text's whitespace-separated words look like a path
    /// (see `render::is_path_like_word`).
    pub shape_path_tokens: i64,
    /// Whether the resolved workflow writes hidden tests: a step whose
    /// action is `"tests"` (directly, or through composition).
    pub shape_tdd: bool,
    /// The repository's own `[checks]` count at enqueue time, from
    /// `forge.toml` (or `.forge/forge.toml`); 0 for a task enqueued before
    /// this column existed, since backfilling it needs the repository's
    /// config as it stood at the time, which the record does not keep.
    pub shape_declared_checks: i64,
}

/// One task in a lineage: parent is what it retries.
#[derive(Debug, Clone)]
pub struct LineageRow {
    pub id: i64,
    pub parent: Option<i64>,
    pub state: String,
    pub reason: String,
    pub workflow: String,
    pub cost: f64,
}

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

/// Task-shape backfill (see docs/ECONOMIST.md, "Task shape"): every task
/// enqueued before `shape_text_len` and its neighbors existed gets them
/// recomputed from what its own row already carries — its text, and its
/// workflow's own stored text — never the filesystem, since a migration
/// only has the connection, and a workflow file may since have changed or
/// gone. Nested composition (a workflow that names another rather than
/// declaring `"tests"` itself, e.g. `tdd-reviewed`) resolves against
/// `known`, seeded with the built-ins and every distinct workflow text
/// already on hand in the table, so a task's own history can stand in for
/// a workflow that changed since. `shape_declared_checks` has no such
/// source — it needs the repository's `[checks]` as it stood at enqueue
/// time — and is left at its column default, 0.
pub(super) fn backfill_task_shape(conn: &Connection) -> rusqlite::Result<()> {
    let mut known: BTreeMap<String, String> = crate::workflows::BUILTIN_WORKFLOWS
        .iter()
        .map(|(file, text)| (file.trim_end_matches(".toml").to_string(), (*text).to_string()))
        .collect();
    {
        let mut stmt = conn
            .prepare("SELECT DISTINCT workflow, workflow_text FROM tasks WHERE workflow_text != ''")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get("workflow")?, r.get("workflow_text")?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (name, text) in rows {
            known.entry(name).or_insert(text);
        }
    }
    let rows: Vec<(i64, String, String, String)> = {
        let mut stmt = conn.prepare("SELECT id, task, workflow, workflow_text FROM tasks")?;
        stmt.query_map([], |r| {
            Ok((
                r.get("id")?,
                r.get("task")?,
                r.get("workflow")?,
                r.get("workflow_text")?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    for (id, text, workflow, workflow_text) in rows {
        let text_len = text.chars().count() as i64;
        let path_tokens = text
            .split_whitespace()
            .filter(|w| crate::render::is_path_like_word(w))
            .count() as i64;
        let source = if workflow_text.is_empty() {
            known.get(&workflow).cloned()
        } else {
            Some(workflow_text)
        };
        let tdd = source
            .is_some_and(|t| crate::workflows::text_writes_hidden_tests(&workflow, &t, &known, 0));
        conn.execute(
            "UPDATE tasks SET shape_text_len=?1, shape_path_tokens=?2, shape_tdd=?3 WHERE id=?4",
            params![text_len, path_tokens, tdd as i64, id],
        )?;
    }
    Ok(())
}

impl Store {
    pub fn insert_task(&self, t: &Task) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO tasks (repo, task, title, base_branch, model, provider, max_turns, max_attempts, timeout_secs, checks_json,
                                state, created_at, budget_usd, allow_protected, workflow, show_checks, workflow_hash, workflow_text, land, after_json, retry_of, journal, context_enabled, resume_on_failure, journal_arm, explore_json,
                                shape_text_len, shape_path_tokens, shape_tdd, shape_declared_checks)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30)",
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
                t.shape_text_len,
                t.shape_path_tokens,
                t.shape_tdd as i64,
                t.shape_declared_checks,
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
             title=?47, landed_at=?48, hand_landed=?49, shape_text_len=?50, shape_path_tokens=?51,
             shape_tdd=?52, shape_declared_checks=?53 WHERE id=?1",
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
                t.shape_text_len,
                t.shape_path_tokens,
                t.shape_tdd as i64,
                t.shape_declared_checks,
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
        t.shape_text_len = 42;
        t.shape_path_tokens = 3;
        t.shape_tdd = true;
        t.shape_declared_checks = 5;
        store.update_task(&t).unwrap();
        let back = store.task(t.id).unwrap().unwrap();
        assert_eq!(format!("{back:?}"), format!("{t:?}"));
    }

    #[test]
    fn migration_backfills_task_shape_from_stored_text_and_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let builtin = |file: &str| {
            crate::workflows::BUILTIN_WORKFLOWS
                .iter()
                .find(|(f, _)| *f == file)
                .unwrap()
                .1
        };
        {
            let c = Connection::open(&path).unwrap();
            for sql in &MIGRATIONS[..(super::TASK_SHAPE_MIGRATION_VERSION as usize - 1)] {
                c.execute_batch(sql).unwrap();
            }
            c.execute_batch(&format!(
                "PRAGMA user_version={}",
                super::TASK_SHAPE_MIGRATION_VERSION - 1
            ))
            .unwrap();
            // A bare tdd task: hidden tests declared directly in its own
            // stored workflow text, and a request naming two paths.
            c.execute(
                "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs, state, created_at, workflow, workflow_text)
                 VALUES ('r', 'fix src/queue.rs and src/store/mod.rs', 'main', 'sonnet', 10, 1, 60, 'succeeded', 1, 'tdd', ?1)",
                params![builtin("tdd.toml")],
            )
            .unwrap();
            // A composed workflow that nests tdd, without this table ever
            // having run bare "tdd" itself: only the compiled-in built-ins
            // let it resolve.
            c.execute(
                "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs, state, created_at, workflow, workflow_text)
                 VALUES ('r', 'polish the docs', 'main', 'sonnet', 10, 1, 60, 'succeeded', 2, 'tdd-reviewed', ?1)",
                params![builtin("tdd-reviewed.toml")],
            )
            .unwrap();
            // No hidden tests, no path-like tokens.
            c.execute(
                "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs, state, created_at, workflow, workflow_text)
                 VALUES ('r', 'rename the button', 'main', 'sonnet', 10, 1, 60, 'succeeded', 3, 'direct', ?1)",
                params![builtin("direct.toml")],
            )
            .unwrap();
        }

        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len() as i64);

        let t1 = s.task(1).unwrap().unwrap();
        assert_eq!(
            t1.shape_text_len,
            "fix src/queue.rs and src/store/mod.rs".chars().count() as i64
        );
        assert_eq!(t1.shape_path_tokens, 2, "two paths named, counts two");
        assert!(t1.shape_tdd, "declares the tests step directly");
        assert_eq!(
            t1.shape_declared_checks, 0,
            "no source to backfill this from"
        );

        let t2 = s.task(2).unwrap().unwrap();
        assert!(t2.shape_tdd, "nests tdd, resolved from the built-ins");

        let t3 = s.task(3).unwrap().unwrap();
        assert!(!t3.shape_tdd, "direct writes no hidden tests");
        assert_eq!(t3.shape_path_tokens, 0);
    }
}
