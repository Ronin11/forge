use super::*;

pub struct WorkflowStat {
    pub workflow: String,
    pub hash: String,
    pub tasks: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub blocked: i64,
    pub unverified: i64,
    pub cost: f64,
    pub attempts: i64,
    pub landed: i64,
    /// Landed tasks whose `landed_sha` became a later task's `base_sha`,
    /// where that later task's first `code` attempt carries a failing L1
    /// verdict row: the base was already broken when the next task started.
    pub broke_base: i64,
    /// Landed tasks named by another task's `repairs` reference
    /// (`forge://task/<id>`).
    pub repaired: i64,
    /// Delayed cost, summed across this workflow's landed tasks (the
    /// `task_repair_cost` cache): for each later landing on the same
    /// repository within `THIRTY_DAYS_SECS`, the fraction of its cost
    /// equal to the lines it removed or rewrote that this landed task
    /// added, divided by all the lines it removed or rewrote. A later
    /// landing that rewrote none of a task's lines charges it nothing; one
    /// that rewrote all of it and nothing else charges it in full. A later
    /// landing following on from two landed tasks in this workflow has its
    /// cost split between them by how many of each task's lines it
    /// rewrote.
    pub repair_cost: f64,
    /// Lines this workflow's landed tasks added, summed from the
    /// `task_churn` cache.
    pub added_lines: i64,
    /// Of `added_lines`, how many a later landing on the same repository
    /// removed or rewrote within `THIRTY_DAYS_SECS` (`task_churn`), summed.
    pub churned_lines: i64,
}

/// Human attention for one workflow version: what a person had to do for
/// its landed work, since minutes cannot be measured (docs/LATER.md, "Two
/// metrics the record can compute and does not"). Four signals, summed as
/// `events` and divided by `landed` to give `events_per_landed`:
/// `operator_answers` (decisions on this workflow's tasks with
/// `answered_by` other than `"supervisor"`), `hand_landed` (this
/// workflow's tasks landed by a human's `forge land`), `withdrawals`
/// (this workflow's tasks left `withdrawn`), and `hand_commits` (commits
/// not authored as Forge, on the base branch, between this workflow's
/// landings and the ones before them — the `task_hand_commits` cache,
/// summed the same way `WorkflowStat::repair_cost` sums `task_repair_cost`).
pub struct HumanAttentionStat {
    pub workflow: String,
    pub hash: String,
    pub landed: i64,
    pub operator_answers: i64,
    pub hand_landed: i64,
    pub withdrawals: i64,
    pub hand_commits: i64,
}

/// Human attention for one project: the same four signals as
/// `HumanAttentionStat`, over a project's tasks instead of one workflow
/// version's.
pub struct HumanAttentionProjectStat {
    pub project: String,
    pub landed: i64,
    pub operator_answers: i64,
    pub hand_landed: i64,
    pub withdrawals: i64,
    pub hand_commits: i64,
}

/// One landed task's time to live: how long the request took to go live
/// (docs/LATER.md, "Two metrics the record can compute and does not").
/// `secs` is `landed_at - created_at`, or, when a deploy ran on behalf of
/// this task, that deploy's `finished_at - created_at` instead — going
/// live means the deploy, not just the landing, once one is tied to the
/// task. `view::time_to_live` turns a scope's worth of these into the
/// median and 90th percentile, per workflow and per project.
pub struct TaskTtl {
    pub workflow: String,
    pub hash: String,
    pub project: Option<String>,
    pub secs: i64,
}

pub struct StepStat {
    pub workflow: String,
    pub step: String,
    pub attempts: i64,
    pub succeeded: i64,
    pub agent_failed: i64,
    pub checks_failed: i64,
    pub needs_input: i64,
    pub mean_turns: f64,
    pub cost: f64,
    pub mean_ms: f64,
    /// Mean tool calls before the first edit, over attempts that edited.
    pub mean_first_edit: Option<f64>,
    /// Mean input tokens, over attempts that reported usage.
    pub mean_input_tokens: Option<f64>,
}

/// One side of the journal control arm's retrospective split: code attempts
/// after the first (`attempt_no > 1`), grouped by whether `inputs_json`'s
/// `journal` field was present and non-empty. See docs/LATER.md, "The
/// journal measurement was ill-posed three times".
pub struct JournalStat {
    pub has_journal: bool,
    pub attempts: i64,
    pub succeeded: i64,
    pub mean_turns: f64,
    /// Mean tool calls before the first edit, over attempts that edited.
    pub mean_first_edit: Option<f64>,
    /// Mean cost in USD per attempt.
    pub mean_cost_usd: f64,
}

/// One row of the runner breakdown: attempts, outcomes, cost and wall
/// time for one (role, provider, model) combination, role being the
/// attempt's step (see `forge stats --by-role`).
pub struct RoleStat {
    pub role: String,
    pub provider: String,
    pub model: String,
    pub attempts: i64,
    pub succeeded: i64,
    pub mean_turns: f64,
    pub mean_cost_usd: f64,
    pub mean_ms: f64,
    /// Landed tasks with an attempt in this group, and how many broke a
    /// later task's base (see `WorkflowStat::broke_base`); `None` outside
    /// the `code` role, where landing is not meaningful.
    pub landed: Option<i64>,
    pub broke_base: Option<i64>,
    /// See `WorkflowStat::repair_cost`, summed over this group's own
    /// landed tasks; `None` outside the `code` role.
    pub repair_cost: Option<f64>,
    /// See `WorkflowStat::added_lines`/`churned_lines`, summed over this
    /// group's own landed tasks; `None` outside the `code` role.
    pub added_lines: Option<i64>,
    pub churned_lines: Option<i64>,
}

/// What `forge stats` filters on: the same project/initiative scope as
/// `TaskFilter` and `DecisionFilter`, without the paging/grep fields that
/// only `forge log`'s raw listing needs.
#[derive(Default, Debug, Clone)]
pub struct StatsFilter {
    pub project: Option<String>,
    pub initiative: Option<i64>,
}

/// `task_repair_cost`'s cached row for `task_id`: `(repair_cost,
/// computed_at)`, or `None` if it has never been computed. See
/// `compute_repair_cost` in view.rs for how the git-level number is
/// derived.
fn repair_cost_cache_query(c: &Connection, task_id: i64) -> Result<Option<(f64, i64)>> {
    Ok(c.query_row(
        "SELECT repair_cost, computed_at FROM task_repair_cost WHERE task_id = ?1",
        params![task_id],
        |r| Ok((r.get("repair_cost")?, r.get("computed_at")?)),
    )
    .optional()?)
}

fn set_repair_cost_cache_query(
    c: &Connection,
    task_id: i64,
    repair_cost: f64,
    computed_at: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO task_repair_cost (task_id, repair_cost, computed_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(task_id) DO UPDATE SET
           repair_cost = excluded.repair_cost,
           computed_at = excluded.computed_at",
        params![task_id, repair_cost, computed_at],
    )?;
    Ok(())
}

/// The cached line-overlap between an earlier landing (`t_sha`, the
/// landed commit whose lines were added) and a later one (`l_sha`, the
/// landed commit that removed or rewrote lines): `(overlap_lines,
/// removed_lines)`, where `overlap_lines` is how many lines `t_sha`'s
/// landing added that `l_sha`'s landing removed or rewrote, and
/// `removed_lines` is how many lines `l_sha`'s landing removed or rewrote
/// in total. Keyed by both landed commits, not task ids, since the
/// underlying git diffs never change once a task has landed.
fn line_overlap_cache_query(
    c: &Connection,
    t_sha: &str,
    l_sha: &str,
) -> Result<Option<(i64, i64)>> {
    Ok(c.query_row(
        "SELECT overlap_lines, removed_lines FROM line_overlap_cache WHERE t_sha = ?1 AND l_sha = ?2",
        params![t_sha, l_sha],
        |r| Ok((r.get("overlap_lines")?, r.get("removed_lines")?)),
    )
    .optional()?)
}

fn set_line_overlap_cache_query(
    c: &Connection,
    t_sha: &str,
    l_sha: &str,
    overlap_lines: i64,
    removed_lines: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO line_overlap_cache (t_sha, l_sha, overlap_lines, removed_lines)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(t_sha, l_sha) DO UPDATE SET
           overlap_lines = excluded.overlap_lines,
           removed_lines = excluded.removed_lines",
        params![t_sha, l_sha, overlap_lines, removed_lines],
    )?;
    Ok(())
}

/// The (provider, model) pairs `task_id` ran a `code` attempt under: which
/// `by_role` groups a landed task's delayed cost is attributed to.
fn code_attempt_groups_query(c: &Connection, task_id: i64) -> Result<Vec<(String, String)>> {
    let mut stmt = c.prepare(
        "SELECT DISTINCT provider, COALESCE(json_extract(inputs_json, '$.model'), '') AS model
         FROM attempts WHERE task_id = ?1 AND step = 'code'",
    )?;
    let rows = stmt.query_map(params![task_id], |r| {
        Ok((r.get("provider")?, r.get("model")?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `task_churn`'s cached row for `task_id`: `(added_lines, churned_lines,
/// computed_at)`, or `None` if it has never been computed.
fn churn_cache_query(c: &Connection, task_id: i64) -> Result<Option<(i64, i64, i64)>> {
    Ok(c.query_row(
        "SELECT added_lines, churned_lines, computed_at FROM task_churn WHERE task_id = ?1",
        params![task_id],
        |r| {
            Ok((
                r.get("added_lines")?,
                r.get("churned_lines")?,
                r.get("computed_at")?,
            ))
        },
    )
    .optional()?)
}

fn set_churn_cache_query(
    c: &Connection,
    task_id: i64,
    added_lines: i64,
    churned_lines: i64,
    computed_at: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO task_churn (task_id, added_lines, churned_lines, computed_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(task_id) DO UPDATE SET
           added_lines = excluded.added_lines,
           churned_lines = excluded.churned_lines,
           computed_at = excluded.computed_at",
        params![task_id, added_lines, churned_lines, computed_at],
    )?;
    Ok(())
}

/// `task_hand_commits`'s cached row for `task_id`: hand commits (author
/// not Forge's identity) on the base branch between the previous landing
/// on the same repository and this task's `base_sha`, or `None` if it has
/// never been computed. See `refresh_hand_commits` in view.rs.
fn hand_commits_cache_query(c: &Connection, task_id: i64) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT hand_commits FROM task_hand_commits WHERE task_id = ?1",
        params![task_id],
        |r| r.get(0),
    )
    .optional()?)
}

fn set_hand_commits_cache_query(
    c: &Connection,
    task_id: i64,
    hand_commits: i64,
    computed_at: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO task_hand_commits (task_id, hand_commits, computed_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(task_id) DO UPDATE SET
           hand_commits = excluded.hand_commits,
           computed_at = excluded.computed_at",
        params![task_id, hand_commits, computed_at],
    )?;
    Ok(())
}

impl Store {
    /// `task_churn`'s cached row for `task_id`, if it has been computed.
    pub fn churn_cache(&self, task_id: i64) -> Result<Option<(i64, i64, i64)>> {
        churn_cache_query(&self.lock(), task_id)
    }

    /// Write (or overwrite) `task_id`'s cached churn: lines it added and,
    /// of those, how many a later landing removed or rewrote within the
    /// window, as of `computed_at`.
    pub fn set_churn_cache(
        &self,
        task_id: i64,
        added_lines: i64,
        churned_lines: i64,
        computed_at: i64,
    ) -> Result<()> {
        set_churn_cache_query(
            &self.lock(),
            task_id,
            added_lines,
            churned_lines,
            computed_at,
        )
    }

    /// `task_repair_cost`'s cached row for `task_id`, if it has been
    /// computed.
    pub fn repair_cost_cache(&self, task_id: i64) -> Result<Option<(f64, i64)>> {
        repair_cost_cache_query(&self.lock(), task_id)
    }

    /// Write (or overwrite) `task_id`'s cached repair cost, as of
    /// `computed_at`.
    pub fn set_repair_cost_cache(
        &self,
        task_id: i64,
        repair_cost: f64,
        computed_at: i64,
    ) -> Result<()> {
        set_repair_cost_cache_query(&self.lock(), task_id, repair_cost, computed_at)
    }

    /// The cached line-overlap between an earlier landing (`t_sha`) and a
    /// later one (`l_sha`), if it has been computed.
    pub fn line_overlap_cache(&self, t_sha: &str, l_sha: &str) -> Result<Option<(i64, i64)>> {
        line_overlap_cache_query(&self.lock(), t_sha, l_sha)
    }

    /// Write (or overwrite) the cached line-overlap between `t_sha` and
    /// `l_sha`.
    pub fn set_line_overlap_cache(
        &self,
        t_sha: &str,
        l_sha: &str,
        overlap_lines: i64,
        removed_lines: i64,
    ) -> Result<()> {
        set_line_overlap_cache_query(&self.lock(), t_sha, l_sha, overlap_lines, removed_lines)
    }

    /// `task_hand_commits`'s cached row for `task_id`, if it has been computed.
    pub fn hand_commits_cache(&self, task_id: i64) -> Result<Option<i64>> {
        hand_commits_cache_query(&self.lock(), task_id)
    }

    /// Write (or overwrite) `task_id`'s cached hand-commit count.
    pub fn set_hand_commits_cache(
        &self,
        task_id: i64,
        hand_commits: i64,
        computed_at: i64,
    ) -> Result<()> {
        set_hand_commits_cache_query(&self.lock(), task_id, hand_commits, computed_at)
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

    /// Workflow versions seen, newest first, by the id of the last task that ran them.
    pub fn workflow_versions(&self, workflow: &str) -> Result<Vec<String>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT workflow_hash FROM tasks WHERE workflow=?1 AND workflow_hash != '' GROUP BY workflow_hash ORDER BY MAX(id) DESC",
        )?;
        let rows = stmt.query_map(params![workflow], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Outcomes per workflow version: the table that compares workflows.
    pub fn workflow_stats(&self, scope: &StatsFilter) -> Result<Vec<WorkflowStat>> {
        let mut stats = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT t.workflow AS workflow, t.workflow_hash AS hash, COUNT(*) AS tasks,
                    SUM(t.state='succeeded') AS succeeded, SUM(t.state='failed') AS failed,
                    SUM(t.state='blocked') AS blocked, SUM(t.state='unverified') AS unverified,
                    COALESCE((SELECT SUM(a.cost_usd) FROM attempts a WHERE a.task_id IN (
                        SELECT id FROM tasks t2 WHERE t2.workflow=t.workflow AND t2.workflow_hash=t.workflow_hash
                          AND (?1 IS NULL OR t2.project = ?1) AND (?2 IS NULL OR t2.initiative = ?2)
                    )), 0) AS cost,
                    COALESCE((SELECT COUNT(*) FROM attempts a WHERE a.task_id IN (
                        SELECT id FROM tasks t2 WHERE t2.workflow=t.workflow AND t2.workflow_hash=t.workflow_hash
                          AND (?1 IS NULL OR t2.project = ?1) AND (?2 IS NULL OR t2.initiative = ?2)
                    )), 0) AS attempts,
                    SUM(t.landed_sha != '') AS landed,
                    SUM(t.landed_sha != '' AND EXISTS (
                        SELECT 1 FROM attempts a
                        JOIN tasks b ON b.id = a.task_id
                        WHERE b.base_sha = t.landed_sha
                          AND a.step = 'code'
                          AND a.attempt_no = (SELECT MIN(a2.attempt_no) FROM attempts a2 WHERE a2.task_id = a.task_id AND a2.step = 'code')
                          AND EXISTS (
                              SELECT 1 FROM json_each(a.verdict_json) j
                              WHERE json_extract(j.value, '$.level') = 'L1' AND json_extract(j.value, '$.ok') = 0
                          )
                    )) AS broke_base,
                    SUM(t.landed_sha != '' AND EXISTS (
                        SELECT 1 FROM task_refs r WHERE r.kind = 'repairs' AND r.url = 'forge://task/' || t.id
                    )) AS repaired
             FROM tasks t WHERE t.state IN ('succeeded','failed','blocked','unverified') AND t.started_at IS NOT NULL
               AND (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.initiative = ?2)
             GROUP BY t.workflow, t.workflow_hash ORDER BY t.workflow, t.workflow_hash",
            )?;
            let rows = stmt.query_map(params![scope.project, scope.initiative], |r| {
                Ok(WorkflowStat {
                    workflow: r.get("workflow")?,
                    hash: r.get("hash")?,
                    tasks: r.get("tasks")?,
                    succeeded: r.get("succeeded")?,
                    failed: r.get("failed")?,
                    blocked: r.get("blocked")?,
                    unverified: r.get("unverified")?,
                    cost: r.get("cost")?,
                    attempts: r.get("attempts")?,
                    landed: r.get("landed")?,
                    broke_base: r.get("broke_base")?,
                    repaired: r.get("repaired")?,
                    repair_cost: 0.0,
                    added_lines: 0,
                    churned_lines: 0,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<WorkflowStat>>>()?
        };
        let landed = self.landed_tasks(scope)?;
        let c = self.lock();
        for t in &landed {
            let Some(w) = stats
                .iter_mut()
                .find(|w| w.workflow == t.workflow && w.hash == t.workflow_hash)
            else {
                continue;
            };
            if let Some((cost, _)) = repair_cost_cache_query(&c, t.id)? {
                w.repair_cost += cost;
            }
            if let Some((added, churned, _)) = churn_cache_query(&c, t.id)? {
                w.added_lines += added;
                w.churned_lines += churned;
            }
        }
        Ok(stats)
    }

    /// Time to live for this scope's landed tasks (see `TaskTtl`): one row
    /// per landed task that already has an answer — its own `landed_at`,
    /// or, when an on-landing deploy ran on its behalf and has finished, that
    /// deploy's `finished_at`. A landed task with a deploy still running
    /// (`finished_at` still `None`) is left out until it finishes, rather
    /// than counted early against its mere landing.
    pub fn task_ttls(&self, scope: &StatsFilter) -> Result<Vec<TaskTtl>> {
        let mut out = Vec::new();
        for t in self.landed_tasks(scope)? {
            let deploys = self.deploys_for_task(t.id)?;
            let end = if let Some(d) = deploys.first() {
                d.finished_at
            } else {
                t.landed_at
            };
            if let Some(end) = end {
                out.push(TaskTtl {
                    workflow: t.workflow.clone(),
                    hash: t.workflow_hash.clone(),
                    project: t.project.clone(),
                    secs: end - t.created_at,
                });
            }
        }
        Ok(out)
    }

    /// Human attention per workflow version: what a person had to do for
    /// its landed work (see `HumanAttentionStat`). One row per workflow +
    /// hash with at least one task in scope, whatever its state — unlike
    /// `workflow_stats`, a workflow whose only tasks were withdrawn still
    /// gets a row here, since a withdrawal is itself a human-attention
    /// signal.
    pub fn human_attention_stats(&self, scope: &StatsFilter) -> Result<Vec<HumanAttentionStat>> {
        let mut stats = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT t.workflow AS workflow, t.workflow_hash AS hash, SUM(t.landed_sha != '') AS landed,
                    COALESCE((SELECT COUNT(*) FROM decisions d JOIN tasks dt ON dt.id = d.task_id
                        WHERE dt.workflow = t.workflow AND dt.workflow_hash = t.workflow_hash
                          AND d.answered_by != 'supervisor'
                          AND (?1 IS NULL OR dt.project = ?1) AND (?2 IS NULL OR dt.initiative = ?2)
                    ), 0) AS operator_answers,
                    SUM(t.hand_landed) AS hand_landed, SUM(t.state = 'withdrawn') AS withdrawals
             FROM tasks t WHERE (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.initiative = ?2)
             GROUP BY t.workflow, t.workflow_hash ORDER BY t.workflow, t.workflow_hash",
            )?;
            let rows = stmt.query_map(params![scope.project, scope.initiative], |r| {
                Ok(HumanAttentionStat {
                    workflow: r.get("workflow")?,
                    hash: r.get("hash")?,
                    landed: r.get("landed")?,
                    operator_answers: r.get("operator_answers")?,
                    hand_landed: r.get("hand_landed")?,
                    withdrawals: r.get("withdrawals")?,
                    hand_commits: 0,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<HumanAttentionStat>>>()?
        };
        let landed = self.landed_tasks(scope)?;
        let c = self.lock();
        for t in &landed {
            let Some(w) = stats
                .iter_mut()
                .find(|w| w.workflow == t.workflow && w.hash == t.workflow_hash)
            else {
                continue;
            };
            if let Some(hand_commits) = hand_commits_cache_query(&c, t.id)? {
                w.hand_commits += hand_commits;
            }
        }
        Ok(stats)
    }

    /// Outcomes per workflow step.
    pub fn step_stats(&self, scope: &StatsFilter) -> Result<Vec<StepStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.workflow AS workflow, a.step AS step, COUNT(*) AS attempts,
                    SUM(a.state='succeeded') AS succeeded, SUM(a.state='agent_failed') AS agent_failed,
                    SUM(a.state='checks_failed') AS checks_failed, SUM(a.state='needs_input') AS needs_input,
                    AVG(a.num_turns) AS mean_turns, COALESCE(SUM(a.cost_usd),0) AS cost, AVG(a.agent_ms) AS mean_ms,
                    AVG(a.first_edit) AS mean_first_edit, AVG(a.input_tokens) AS mean_input_tokens
             FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.state != 'running'
               AND (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.initiative = ?2)
             GROUP BY t.workflow, a.step ORDER BY t.workflow, a.step",
        )?;
        let rows = stmt.query_map(params![scope.project, scope.initiative], |r| {
            Ok(StepStat {
                workflow: r.get("workflow")?,
                step: r.get("step")?,
                attempts: r.get("attempts")?,
                succeeded: r.get("succeeded")?,
                agent_failed: r.get("agent_failed")?,
                checks_failed: r.get("checks_failed")?,
                needs_input: r.get("needs_input")?,
                mean_turns: r.get("mean_turns")?,
                cost: r.get("cost")?,
                mean_ms: r.get("mean_ms")?,
                mean_first_edit: r.get("mean_first_edit")?,
                mean_input_tokens: r.get("mean_input_tokens")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The journal control arm's retrospective split, over code attempts
    /// after the first: one row for attempts handed a journal
    /// (`inputs_json`'s `journal` field present and non-empty), one for
    /// attempts that were not. A side with no matching attempts is omitted.
    pub fn journal_control_stats(&self) -> Result<Vec<JournalStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT
                json_extract(a.inputs_json, '$.journal') IS NOT NULL
                    AND json_extract(a.inputs_json, '$.journal') != '' AS has_journal,
                COUNT(*) AS attempts, SUM(a.state='succeeded') AS succeeded,
                AVG(a.num_turns) AS mean_turns, AVG(a.first_edit) AS mean_first_edit,
                COALESCE(AVG(a.cost_usd), 0) AS mean_cost_usd
             FROM attempts a
             WHERE a.step = 'code' AND a.attempt_no > 1 AND a.state != 'running'
             GROUP BY has_journal",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(JournalStat {
                has_journal: r.get("has_journal")?,
                attempts: r.get("attempts")?,
                succeeded: r.get("succeeded")?,
                mean_turns: r.get("mean_turns")?,
                mean_first_edit: r.get("mean_first_edit")?,
                mean_cost_usd: r.get("mean_cost_usd")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The runner breakdown: attempts, outcomes, cost and wall time per
    /// (role, provider, model), role being the attempt's step. For the
    /// `code` role only, also the landed count and the broke-base count
    /// (see `WorkflowStat::broke_base`) for tasks with an attempt in the
    /// group. `investigate` and `interview` are read-only directives whose
    /// job is to ask when the record does not settle it; an attempt of
    /// either that ended `needs_input` with a plain question counts as a
    /// success here (see `forge stats --by-role`'s footnote).
    pub fn role_stats(&self) -> Result<Vec<RoleStat>> {
        let mut stats = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT a.step AS role, a.provider AS provider, COALESCE(json_extract(a.inputs_json, '$.model'), '') AS attempt_model,
                    COUNT(*) AS attempts,
                    SUM(CASE
                        WHEN a.state='succeeded' THEN 1
                        WHEN a.step IN ('investigate', 'interview') AND a.state='needs_input'
                            AND a.envelope_json != '' AND json_valid(a.envelope_json)
                            AND COALESCE(json_extract(a.envelope_json, '$.needs_input.kind'), 'question') = 'question'
                        THEN 1
                        ELSE 0
                    END) AS succeeded,
                    AVG(a.num_turns) AS mean_turns, COALESCE(AVG(a.cost_usd), 0) AS mean_cost_usd, AVG(a.agent_ms) AS mean_ms,
                    COUNT(DISTINCT CASE WHEN t.landed_sha != '' THEN t.id END) AS landed,
                    COUNT(DISTINCT CASE WHEN t.landed_sha != '' AND EXISTS (
                        SELECT 1 FROM attempts a2
                        JOIN tasks b ON b.id = a2.task_id
                        WHERE b.base_sha = t.landed_sha
                          AND a2.step = 'code'
                          AND a2.attempt_no = (SELECT MIN(a3.attempt_no) FROM attempts a3 WHERE a3.task_id = a2.task_id AND a3.step = 'code')
                          AND EXISTS (
                              SELECT 1 FROM json_each(a2.verdict_json) j
                              WHERE json_extract(j.value, '$.level') = 'L1' AND json_extract(j.value, '$.ok') = 0
                          )
                    ) THEN t.id END) AS broke_base
             FROM attempts a JOIN tasks t ON t.id = a.task_id
             WHERE a.state != 'running'
             GROUP BY a.step, a.provider, attempt_model
             ORDER BY a.step, a.provider, attempt_model",
            )?;
            let rows = stmt.query_map([], |r| {
                let role: String = r.get("role")?;
                let landed: i64 = r.get("landed")?;
                let broke_base: i64 = r.get("broke_base")?;
                let is_code = role == "code";
                Ok(RoleStat {
                    role,
                    provider: r.get("provider")?,
                    model: r.get("attempt_model")?,
                    attempts: r.get("attempts")?,
                    succeeded: r.get("succeeded")?,
                    mean_turns: r.get("mean_turns")?,
                    mean_cost_usd: r.get("mean_cost_usd")?,
                    mean_ms: r.get("mean_ms")?,
                    landed: is_code.then_some(landed),
                    broke_base: is_code.then_some(broke_base),
                    repair_cost: is_code.then_some(0.0),
                    added_lines: is_code.then_some(0),
                    churned_lines: is_code.then_some(0),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<RoleStat>>>()?
        };
        let landed = self.landed_tasks(&StatsFilter::default())?;
        let c = self.lock();
        for t in &landed {
            let groups = code_attempt_groups_query(&c, t.id)?;
            if groups.is_empty() {
                continue;
            }
            let cost = repair_cost_cache_query(&c, t.id)?.map(|(cost, _)| cost);
            let churn = churn_cache_query(&c, t.id)?;
            for (provider, model) in groups {
                let Some(r) = stats
                    .iter_mut()
                    .find(|r| r.role == "code" && r.provider == provider && r.model == model)
                else {
                    continue;
                };
                if let Some(cost) = cost {
                    r.repair_cost = Some(r.repair_cost.unwrap_or(0.0) + cost);
                }
                if let Some((added, churned, _)) = churn {
                    r.added_lines = Some(r.added_lines.unwrap_or(0) + added);
                    r.churned_lines = Some(r.churned_lines.unwrap_or(0) + churned);
                }
            }
        }
        Ok(stats)
    }

    /// Human attention per project: what a person had to do for its
    /// landed work (see `HumanAttentionStat`), same scoping rule as
    /// `project_stats` (only every project's own tasks, whatever their
    /// state — a project with only withdrawn tasks still gets a row).
    pub fn human_attention_project_stats(&self) -> Result<Vec<HumanAttentionProjectStat>> {
        let mut stats = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT t.project AS project, SUM(t.landed_sha != '') AS landed,
                    COALESCE((SELECT COUNT(*) FROM decisions d JOIN tasks dt ON dt.id = d.task_id
                        WHERE dt.project = t.project AND d.answered_by != 'supervisor'
                    ), 0) AS operator_answers,
                    SUM(t.hand_landed) AS hand_landed, SUM(t.state = 'withdrawn') AS withdrawals
             FROM tasks t WHERE t.project IS NOT NULL
             GROUP BY t.project ORDER BY t.project",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(HumanAttentionProjectStat {
                    project: r.get("project")?,
                    landed: r.get("landed")?,
                    operator_answers: r.get("operator_answers")?,
                    hand_landed: r.get("hand_landed")?,
                    withdrawals: r.get("withdrawals")?,
                    hand_commits: 0,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<HumanAttentionProjectStat>>>()?
        };
        let landed = self.landed_tasks(&StatsFilter::default())?;
        let c = self.lock();
        for t in &landed {
            let Some(project) = &t.project else {
                continue;
            };
            let Some(p) = stats.iter_mut().find(|p| &p.project == project) else {
                continue;
            };
            if let Some(hand_commits) = hand_commits_cache_query(&c, t.id)? {
                p.hand_commits += hand_commits;
            }
        }
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defect_escape_counts_broke_base_and_repaired_once_each() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();

        let base_task = |started_at: i64| Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: started_at,
            started_at: Some(started_at),
            finished_at: Some(started_at + 1),
            workflow: "direct".into(),
            ..Default::default()
        };

        // A lands.
        let mut a = base_task(1);
        a.id = s.insert_task(&a).unwrap();
        a.landed_sha = "aaaaaaaa".into();
        s.update_task(&a).unwrap();

        // B starts from A's landed sha, and its first (and only) code
        // attempt is red on that base: an L1 row fails before B has done
        // anything of its own.
        let mut b = base_task(2);
        b.base_sha = "aaaaaaaa".into();
        b.id = s.insert_task(&b).unwrap();
        s.update_task(&b).unwrap();
        let b_attempt = Attempt {
            task_id: b.id,
            attempt_no: 1,
            step: "code".into(),
            started_at: 2,
            ..Default::default()
        };
        let b_attempt_id = s.insert_attempt(&b_attempt).unwrap();
        s.finish_attempt(&FinishAttempt {
            id: b_attempt_id,
            state: AttemptState::ChecksFailed,
            reason: "L1 failed: test".into(),
            finished_at: Some(3),
            agent_exit: Some(0),
            timed_out: false,
            num_turns: 1,
            tool_calls: 1,
            cost_usd: Some(0.0),
            agent_ms: 0,
            commits: 0,
            files_changed: 0,
            dirty: false,
            verdict_json: r#"[{"level":"L1","name":"test","ok":false,"exit":1,"ms":0,"timed_out":false,"tail":"","failing_tests":[]}]"#.into(),
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
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            early_signals: "[]".into(),
            early_near: "[]".into(),
        })
        .unwrap();

        // C carries a repairs reference to A.
        let mut c = base_task(4);
        c.id = s.insert_task(&c).unwrap();
        s.update_task(&c).unwrap();
        s.insert_task_ref(
            c.id,
            "repairs",
            &format!("forge://task/{}", a.id),
            "",
            "operator",
        )
        .unwrap();

        let stats = s.workflow_stats(&StatsFilter::default()).unwrap();
        assert_eq!(stats.len(), 1);
        let w = &stats[0];
        assert_eq!(w.tasks, 3);
        assert_eq!(w.landed, 1, "only A landed");
        assert_eq!(w.broke_base, 1, "A counts once for breaking B's base");
        assert_eq!(w.repaired, 1, "A counts once as repaired by C");
    }

    /// The git-level line-overlap attribution itself (half of a rewritten
    /// task's cost, none for an untouched one) is exercised on a real
    /// fixture repository in view.rs's `stats_tests`, next to the churn
    /// test it shares a fixture style with. This is the SQL half: once
    /// `task_repair_cost` is populated, `workflow_stats` sums it across
    /// every landed task in the workflow, the same way it already sums
    /// `task_churn`.
    #[test]
    fn workflow_stats_sums_the_repair_cost_cache_over_landed_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();

        let landed = |finished_at: i64, landed_sha: &str| Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: finished_at,
            started_at: Some(finished_at),
            finished_at: Some(finished_at),
            workflow: "direct".into(),
            landed_sha: landed_sha.into(),
            ..Default::default()
        };
        let insert = |mut t: Task| {
            t.id = s.insert_task(&t).unwrap();
            s.update_task(&t).unwrap();
            t
        };

        let a = insert(landed(1000, "asha"));
        let b = insert(landed(2000, "bsha"));
        s.set_repair_cost_cache(a.id, 3.5, 9999).unwrap();
        s.set_repair_cost_cache(b.id, 1.5, 9999).unwrap();

        let stats = s.workflow_stats(&StatsFilter::default()).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].repair_cost, 5.0, "3.5 + 1.5, cached per task");
    }

    /// Fixture: two landed tasks (one landed the ordinary way, one by hand
    /// and later deployed) plus a withdrawn one, all in the same workflow
    /// and project. Exercises both new metrics end to end at the store
    /// level: human attention's four signals (operator answers, hand
    /// landings, withdrawals, hand commits) summed and divided by landed
    /// pieces, and time to live (a deploy's `finished_at` overriding a
    /// task's own `landed_at` once one is tied to it).
    #[test]
    fn human_attention_and_time_to_live_count_hand_landing_withdrawal_and_deploy() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        s.create_project(&Project {
            name: "proj".into(),
            purpose: "p".into(),
            created_at: 0,
            ..Default::default()
        })
        .unwrap();

        let base = |created_at: i64| Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at,
            started_at: Some(created_at),
            finished_at: Some(created_at + 10),
            workflow: "direct".into(),
            workflow_hash: "h1".into(),
            project: Some("proj".into()),
            ..Default::default()
        };
        let insert = |mut t: Task| {
            t.id = s.insert_task(&t).unwrap();
            s.update_task(&t).unwrap();
            t
        };

        // Task A: landed the ordinary way, no deploy tied to it, and an
        // operator answered a question of its along the way.
        let mut a = base(1000);
        a.landed_sha = "asha".into();
        a.landed_at = Some(1100);
        let a = insert(a);
        s.insert_decision_by(a.id, "r", "q", "operator answered", "operator", "", None)
            .unwrap();

        // Task B: landed by a human's `forge land`, then deployed.
        let mut b = base(2000);
        b.landed_sha = "bsha".into();
        b.landed_at = Some(2500);
        b.hand_landed = true;
        let b = insert(b);
        s.set_hand_commits_cache(b.id, 3, 9999).unwrap();
        let deploy_id = s
            .start_deploy("proj", "prod", "bsha", 2500, Some(b.id))
            .unwrap();
        s.finish_deploy(
            deploy_id, 2600, true, "ok", None, "", None, None, None, None,
        )
        .unwrap();

        // Task C: withdrawn, never landed.
        let mut c = base(3000);
        c.state = TaskState::Withdrawn;
        c.finished_at = Some(3010);
        insert(c);

        let scope = StatsFilter::default();
        let human = s.human_attention_stats(&scope).unwrap();
        assert_eq!(human.len(), 1);
        let h = &human[0];
        assert_eq!(h.landed, 2);
        assert_eq!(h.operator_answers, 1);
        assert_eq!(h.hand_landed, 1);
        assert_eq!(h.withdrawals, 1);
        assert_eq!(
            h.hand_commits, 3,
            "cached per landed task, like repair_cost"
        );

        let human_p = s.human_attention_project_stats().unwrap();
        assert_eq!(human_p.len(), 1);
        let hp = &human_p[0];
        assert_eq!(hp.project, "proj");
        assert_eq!(hp.landed, 2);
        assert_eq!(hp.operator_answers, 1);
        assert_eq!(hp.hand_landed, 1);
        assert_eq!(hp.withdrawals, 1);
        assert_eq!(hp.hand_commits, 3);

        let mut ttls = s.task_ttls(&scope).unwrap();
        ttls.sort_by_key(|t| t.secs);
        assert_eq!(ttls.len(), 2);
        assert_eq!(ttls[0].secs, 100, "task A: landed_at - created_at");
        assert_eq!(
            ttls[1].secs, 600,
            "task B: the tied deploy's finished_at - created_at, not landed_at"
        );
    }

    #[test]
    fn journal_control_stats_splits_code_retries_by_whether_the_journal_was_shown() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let t = Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 4,
            timeout_secs: 1,
            ..Default::default()
        };
        let task_id = s.insert_task(&t).unwrap();

        let attempt = |attempt_no, step: &str, inputs_json: &str| Attempt {
            task_id,
            attempt_no,
            step: step.into(),
            started_at: 0,
            inputs_json: inputs_json.into(),
            ..Default::default()
        };
        let finish = |id, state, num_turns, first_edit, cost_usd| {
            s.finish_attempt(&FinishAttempt {
                id,
                state,
                reason: String::new(),
                finished_at: Some(1),
                agent_exit: Some(0),
                timed_out: false,
                num_turns,
                tool_calls: 1,
                cost_usd: Some(cost_usd),
                agent_ms: 0,
                commits: 1,
                files_changed: 1,
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
                first_edit,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                early_signals: "[]".into(),
                early_near: "[]".into(),
            })
            .unwrap();
        };

        // Two retries handed a journal.
        let a = s
            .insert_attempt(&attempt(
                2,
                "code",
                r#"{"journal":"earlier attempt said..."}"#,
            ))
            .unwrap();
        finish(a, AttemptState::Succeeded, 30, Some(10), 1.0);
        let b = s
            .insert_attempt(&attempt(3, "code", r#"{"journal":"more history"}"#))
            .unwrap();
        finish(b, AttemptState::ChecksFailed, 20, Some(6), 0.5);

        // One retry with no journal (absent field).
        let c = s.insert_attempt(&attempt(2, "code", "{}")).unwrap();
        finish(c, AttemptState::Succeeded, 25, None, 0.6);

        // Excluded: a first attempt (never a retry) even though it carries
        // a journal, and a non-code step's retry.
        let d = s
            .insert_attempt(&attempt(
                1,
                "code",
                r#"{"journal":"ignored, first attempt"}"#,
            ))
            .unwrap();
        finish(d, AttemptState::Succeeded, 99, Some(1), 9.0);
        let e = s
            .insert_attempt(&attempt(
                2,
                "review",
                r#"{"journal":"ignored, wrong step"}"#,
            ))
            .unwrap();
        finish(e, AttemptState::Succeeded, 99, Some(1), 9.0);

        let stats = s.journal_control_stats().unwrap();
        assert_eq!(stats.len(), 2);
        let journal = stats.iter().find(|j| j.has_journal).expect("a journal row");
        assert_eq!(journal.attempts, 2);
        assert_eq!(journal.succeeded, 1);
        assert_eq!(journal.mean_turns, 25.0);
        assert_eq!(journal.mean_first_edit, Some(8.0));
        assert_eq!(journal.mean_cost_usd, 0.75);

        let no_journal = stats
            .iter()
            .find(|j| !j.has_journal)
            .expect("a no-journal row");
        assert_eq!(no_journal.attempts, 1);
        assert_eq!(no_journal.succeeded, 1);
        assert_eq!(no_journal.mean_turns, 25.0);
        assert_eq!(
            no_journal.mean_first_edit, None,
            "the only attempt never edited"
        );
        assert_eq!(no_journal.mean_cost_usd, 0.6);
    }

    #[test]
    fn role_stats_splits_by_provider_and_model_and_averages_within_each() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();

        let base_task = |started_at: i64| Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: started_at,
            started_at: Some(started_at),
            finished_at: Some(started_at + 1),
            workflow: "direct".into(),
            ..Default::default()
        };
        let attempt = |task_id, step: &str, provider: &str, model: &str| Attempt {
            task_id,
            attempt_no: 1,
            step: step.into(),
            provider: provider.into(),
            started_at: 0,
            inputs_json: format!(r#"{{"model":"{model}"}}"#),
            ..Default::default()
        };
        let finish = |id, state, num_turns, cost_usd, agent_ms, verdict_json: &str| {
            s.finish_attempt(&FinishAttempt {
                id,
                state,
                reason: String::new(),
                finished_at: Some(1),
                agent_exit: Some(0),
                timed_out: false,
                num_turns,
                tool_calls: 1,
                cost_usd: Some(cost_usd),
                agent_ms,
                commits: 1,
                files_changed: 1,
                dirty: false,
                verdict_json: verdict_json.into(),
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
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                early_signals: "[]".into(),
                early_near: "[]".into(),
            })
            .unwrap();
        };

        // A: code / anthropic / sonnet, succeeds and lands.
        let mut a = base_task(1);
        a.id = s.insert_task(&a).unwrap();
        let a1 = s
            .insert_attempt(&attempt(a.id, "code", "anthropic", "sonnet"))
            .unwrap();
        finish(a1, AttemptState::Succeeded, 10, 1.0, 1000, "[]");
        a.landed_sha = "aaaaaaaa".into();
        s.update_task(&a).unwrap();

        // A also carries a review-step attempt: a different role, excluded
        // from landed/broke-base entirely.
        let a2 = s
            .insert_attempt(&attempt(a.id, "review", "anthropic", "sonnet"))
            .unwrap();
        finish(a2, AttemptState::Succeeded, 2, 0.1, 100, "[]");

        // B: same code / anthropic / sonnet group, fails, never lands.
        let mut b = base_task(2);
        b.id = s.insert_task(&b).unwrap();
        let b1 = s
            .insert_attempt(&attempt(b.id, "code", "anthropic", "sonnet"))
            .unwrap();
        finish(b1, AttemptState::ChecksFailed, 20, 3.0, 3000, "[]");
        s.update_task(&b).unwrap();

        // C: code / openai / gpt-5, its own group entirely, succeeds and lands.
        let mut c = base_task(3);
        c.id = s.insert_task(&c).unwrap();
        let c1 = s
            .insert_attempt(&attempt(c.id, "code", "openai", "gpt-5"))
            .unwrap();
        finish(c1, AttemptState::Succeeded, 5, 0.5, 500, "[]");
        c.landed_sha = "cccccccc".into();
        s.update_task(&c).unwrap();

        // D: code / anthropic / haiku, its own group; starts from A's landed
        // sha and is red on it, so A's group counts a broke-base.
        let mut d = base_task(4);
        d.base_sha = "aaaaaaaa".into();
        d.id = s.insert_task(&d).unwrap();
        let d1 = s
            .insert_attempt(&attempt(d.id, "code", "anthropic", "haiku"))
            .unwrap();
        finish(
            d1,
            AttemptState::ChecksFailed,
            1,
            0.0,
            0,
            r#"[{"level":"L1","name":"test","ok":false,"exit":1,"ms":0,"timed_out":false,"tail":"","failing_tests":[]}]"#,
        );
        s.update_task(&d).unwrap();

        let stats = s.role_stats().unwrap();
        assert_eq!(
            stats.len(),
            4,
            "code/anthropic/sonnet, code/openai/gpt-5, code/anthropic/haiku, review/anthropic/sonnet"
        );

        let find = |role: &str, provider: &str, model: &str| {
            stats
                .iter()
                .find(|r| r.role == role && r.provider == provider && r.model == model)
                .unwrap_or_else(|| panic!("no row for {role}/{provider}/{model}"))
        };

        let sonnet = find("code", "anthropic", "sonnet");
        assert_eq!(sonnet.attempts, 2);
        assert_eq!(sonnet.succeeded, 1);
        assert_eq!(sonnet.mean_turns, 15.0);
        assert_eq!(sonnet.mean_cost_usd, 2.0);
        assert_eq!(sonnet.mean_ms, 2000.0);
        assert_eq!(sonnet.landed, Some(1), "only A landed");
        assert_eq!(sonnet.broke_base, Some(1), "A broke D's base");

        let gpt = find("code", "openai", "gpt-5");
        assert_eq!(gpt.attempts, 1);
        assert_eq!(gpt.succeeded, 1);
        assert_eq!(gpt.mean_turns, 5.0);
        assert_eq!(gpt.mean_cost_usd, 0.5);
        assert_eq!(gpt.mean_ms, 500.0);
        assert_eq!(gpt.landed, Some(1));
        assert_eq!(gpt.broke_base, Some(0), "nothing based off C's landed sha");

        let haiku = find("code", "anthropic", "haiku");
        assert_eq!(haiku.attempts, 1);
        assert_eq!(haiku.succeeded, 0);
        assert_eq!(haiku.landed, Some(0), "D never landed");
        assert_eq!(haiku.broke_base, Some(0));

        let review = find("review", "anthropic", "sonnet");
        assert_eq!(review.attempts, 1);
        assert_eq!(review.succeeded, 1);
        assert_eq!(review.landed, None, "landed is code-only");
        assert_eq!(review.broke_base, None, "broke-base is code-only");
    }

    #[test]
    fn role_stats_counts_an_investigate_or_interview_question_as_a_success() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();

        let mut t = Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Blocked,
            created_at: 1,
            workflow: "direct".into(),
            ..Default::default()
        };
        t.id = s.insert_task(&t).unwrap();

        let attempt = |step: &str| Attempt {
            task_id: t.id,
            attempt_no: 1,
            step: step.into(),
            provider: "anthropic".into(),
            started_at: 0,
            inputs_json: r#"{"model":"m"}"#.into(),
            ..Default::default()
        };
        let question = |kind: &str| {
            format!(
                r#"{{"schema_version":1,"summary":"s","needs_input":{{"question":"q","tried":"t","kind":"{kind}"}},"changes":[],"checks_run":[],"claims":[]}}"#
            )
        };
        let finish = |id, state, envelope_json: &str| {
            s.finish_attempt(&FinishAttempt {
                id,
                state,
                reason: String::new(),
                finished_at: Some(1),
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
                envelope_json: envelope_json.into(),
                rl_five_hour: None,
                rl_seven_day: None,
                rl_five_hour_resets: None,
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
            })
            .unwrap();
        };

        // investigate: asked a plain question, no changes: a success.
        let a1 = s.insert_attempt(&attempt("investigate")).unwrap();
        finish(a1, AttemptState::NeedsInput, &question("question"));

        // interview: the same.
        let a2 = s.insert_attempt(&attempt("interview")).unwrap();
        finish(a2, AttemptState::NeedsInput, &question("question"));

        // investigate that needed a different workflow, not a question it
        // asked: not what this counts.
        let a3 = s.insert_attempt(&attempt("investigate")).unwrap();
        finish(a3, AttemptState::NeedsInput, &question("workflow"));

        // code ending needs_input with a question: not an investigate or
        // interview role, so not counted as a success here.
        let a4 = s.insert_attempt(&attempt("code")).unwrap();
        finish(a4, AttemptState::NeedsInput, &question("question"));

        let stats = s.role_stats().unwrap();
        let find = |role: &str| stats.iter().find(|r| r.role == role).unwrap();

        let investigate = find("investigate");
        assert_eq!(investigate.attempts, 2);
        assert_eq!(
            investigate.succeeded, 1,
            "the plain question counts; the workflow one does not"
        );

        let interview = find("interview");
        assert_eq!(interview.attempts, 1);
        assert_eq!(interview.succeeded, 1);

        let code = find("code");
        assert_eq!(code.attempts, 1);
        assert_eq!(
            code.succeeded, 0,
            "needs_input on a role that is not investigate/interview is not a success"
        );
    }
}
