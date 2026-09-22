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
    /// `"attempt"` for a build task's attempt, `"job_step"` for a job's
    /// directive step (docs/JOBS.md, "Steps"); a job step has no turns, no
    /// success/failure of its own, and is never part of a landing, so those
    /// fields are zeroed or `None` on a `"job_step"` row.
    pub kind: String,
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

/// The roles `Task::routing` (and every `code`/`tests`/`review`/`plan`/
/// `assess` attempt) can name: what `forge stats --factors`'s "provider
/// per role" factor iterates over (see docs/ECONOMIST.md, "The inputs,
/// and where each lives" and "Provider per role").
const ROLES: [&str; 5] = ["code", "tests", "review", "plan", "assess"];

/// Below this, a task's size class is `"small"`; below `SIZE_MEDIUM_MAX`
/// it is `"medium"`; at or above, `"large"` (see `size_class`).
const SIZE_SMALL_MAX: i64 = 150;
const SIZE_MEDIUM_MAX: i64 = 400;

/// Task size at intake, in three bins, for `forge stats --factors` (see
/// docs/ECONOMIST.md, "Task shape"): a plain score over the two shape
/// fields piece 1 recorded, `shape_text_len` (characters) and
/// `shape_path_tokens` (words that look like a path). A path token names
/// a unit of work explicitly, so it counts for more than a character of
/// prose; the weight (80) and the two cutoffs are a hand-picked, round-
/// number split, not a fit — task 194 (docs/LATER.md, "What a capped
/// attempt costs at 100 turns"), which cost five times the mean, scores
/// well past `SIZE_MEDIUM_MAX` on this rule.
pub fn size_class(text_len: i64, path_tokens: i64) -> &'static str {
    let score = text_len + path_tokens * 80;
    if score < SIZE_SMALL_MAX {
        "small"
    } else if score < SIZE_MEDIUM_MAX {
        "medium"
    } else {
        "large"
    }
}

/// Where `size_class`'s three levels sort against each other: intake
/// order, not alphabetical (`"large" < "medium" < "small"` would read
/// backwards).
fn size_rank(level: &str) -> u8 {
    match level {
        "small" => 0,
        "medium" => 1,
        "large" => 2,
        _ => 3,
    }
}

/// One factor level of `forge stats --factors` (docs/ECONOMIST.md,
/// "Provider per role" and "Task size"): a group of tasks sharing one
/// value of one factor over landed and failed tasks in the window —
/// `"provider:<role>"` (one factor per role in `ROLES`, level the
/// provider that role ran under), `"workflow"` (level the workflow
/// name), or `"size"` (level `size_class`'s three bins).
///
/// `tasks`/`landed`/`rate` (with its Wilson 95% interval, `rate_lo`/
/// `rate_hi`) and `mean_true_cost_usd` (attempt cost plus the cached
/// repair cost, over this level's own landed tasks; `None` when none
/// landed) are read straight off this level's own tasks. `effect` and
/// `effect_se` come from the one joint main-effects fit across every
/// factor and level in scope (`fit_main_effects`): the change in
/// log(true cost) this level carries against its factor's reference
/// level (`is_reference`), with its standard error. Both are `None` for
/// the reference level itself, and for every level of a factor the fit
/// dropped — stuck at one level among the landed tasks that carry it,
/// or too little data next to the number of columns in play (`p >= n`),
/// or perfectly confounded with another factor (a singular fit).
pub struct FactorLevelStat {
    pub factor: String,
    pub level: String,
    pub tasks: i64,
    pub landed: i64,
    pub rate: f64,
    pub rate_lo: f64,
    pub rate_hi: f64,
    pub mean_true_cost_usd: Option<f64>,
    pub is_reference: bool,
    pub effect: Option<f64>,
    pub effect_se: Option<f64>,
}

/// One landed task's row in the main-effects design (`fit_main_effects`):
/// the `(factor, level)` pairs it carries — every task has `"workflow"`
/// and `"size"`, plus a `"provider:<role>"` entry for every role it ran
/// — and its response, `ln(true cost)`.
struct FitRow {
    levels: Vec<(String, String)>,
    log_cost: f64,
}

/// Plain least-squares, by hand (docs/ECONOMIST.md, piece 3: "no
/// library, normal equations on a handful of columns"): one dummy
/// column per non-reference `(factor, level)` in `reference`, plus an
/// intercept, fit to `rows`'s `log_cost` by solving the normal
/// equations `(X'X) beta = X'y` via Gauss-Jordan elimination with
/// partial pivoting (`invert`). A row missing a factor `reference`
/// covers (a role that never ran on that task) is 0 on every one of
/// that factor's dummies — indistinguishable from its reference level,
/// not a claim that it ran there.
///
/// Returns, per non-reference `(factor, level)`, its effect and standard
/// error against the reference (`sigma^2 * (X'X)^-1_jj`, `sigma^2` the
/// residual variance). Returns an empty map when there are not enough
/// rows for the columns in play (`p >= n`) or the normal equations are
/// singular (two factors perfectly confounded in `rows`) — a fit that
/// cannot be told apart from another is reported as no fit, not a
/// guess.
fn fit_main_effects(
    rows: &[FitRow],
    reference: &BTreeMap<String, String>,
) -> BTreeMap<(String, String), (f64, f64)> {
    let mut columns: Vec<(String, String)> = Vec::new();
    for r in rows {
        for (f, l) in &r.levels {
            if reference.get(f).is_some_and(|rl| rl != l)
                && !columns.contains(&(f.clone(), l.clone()))
            {
                columns.push((f.clone(), l.clone()));
            }
        }
    }
    columns.sort();
    let p = columns.len() + 1;
    let n = rows.len();
    if n <= p {
        return BTreeMap::new();
    }
    let mut x = vec![vec![0.0; p]; n];
    let mut y = vec![0.0; n];
    for (i, r) in rows.iter().enumerate() {
        x[i][0] = 1.0;
        for (f, l) in &r.levels {
            if let Some(j) = columns.iter().position(|(cf, cl)| cf == f && cl == l) {
                x[i][j + 1] = 1.0;
            }
        }
        y[i] = r.log_cost;
    }
    let mut xtx = vec![vec![0.0; p]; p];
    let mut xty = vec![0.0; p];
    for i in 0..n {
        for a in 0..p {
            xty[a] += x[i][a] * y[i];
            for b in 0..p {
                xtx[a][b] += x[i][a] * x[i][b];
            }
        }
    }
    let Some(inv) = invert(&xtx) else {
        return BTreeMap::new();
    };
    let beta: Vec<f64> = (0..p)
        .map(|a| (0..p).map(|b| inv[a][b] * xty[b]).sum())
        .collect();
    let mut ssr = 0.0;
    for i in 0..n {
        let pred: f64 = (0..p).map(|a| x[i][a] * beta[a]).sum();
        ssr += (y[i] - pred).powi(2);
    }
    let dof = (n - p) as f64;
    let sigma2 = if dof > 0.0 { ssr / dof } else { 0.0 };
    columns
        .into_iter()
        .enumerate()
        .map(|(idx, level)| {
            let j = idx + 1;
            let se = (sigma2 * inv[j][j]).max(0.0).sqrt();
            (level, (beta[j], se))
        })
        .collect()
}

/// `m`'s inverse via Gauss-Jordan elimination with partial pivoting, or
/// `None` when a column's best remaining pivot is too small to trust
/// (`m` singular, or too close to it) — the one guard `fit_main_effects`
/// needs against a design that cannot be solved rather than a wrong
/// answer.
fn invert(m: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let p = m.len();
    let mut a = m.to_vec();
    let mut inv: Vec<Vec<f64>> = (0..p)
        .map(|i| {
            let mut row = vec![0.0; p];
            row[i] = 1.0;
            row
        })
        .collect();
    for col in 0..p {
        let mut pivot_row = col;
        let mut best = a[col][col].abs();
        for (r, row) in a.iter().enumerate().skip(col + 1) {
            if row[col].abs() > best {
                best = row[col].abs();
                pivot_row = r;
            }
        }
        if best < 1e-9 {
            return None;
        }
        a.swap(col, pivot_row);
        inv.swap(col, pivot_row);
        let pivot = a[col][col];
        for v in a[col].iter_mut() {
            *v /= pivot;
        }
        for v in inv[col].iter_mut() {
            *v /= pivot;
        }
        for r in 0..p {
            if r == col {
                continue;
            }
            let factor = a[r][col];
            if factor == 0.0 {
                continue;
            }
            for c in 0..p {
                let ac = a[col][c];
                a[r][c] -= factor * ac;
                let ic = inv[col][c];
                inv[r][c] -= factor * ic;
            }
        }
    }
    Some(inv)
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

    /// The providers that ran `workflow`'s terminal tasks (at `hash`, or at
    /// any version), by name: the split a profile is measured over, since
    /// a rate averaged across providers describes none of them.
    pub fn workflow_providers(&self, workflow: &str, hash: Option<&str>) -> Result<Vec<String>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT DISTINCT provider FROM tasks
             WHERE workflow=?1 AND (?2 IS NULL OR workflow_hash=?2)
               AND state IN ('succeeded','failed','blocked','unverified')
               AND started_at IS NOT NULL
             ORDER BY provider",
        )?;
        let rows = stmt.query_map(params![workflow, hash], |r| r.get(0))?;
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
    ///
    /// Exactly four signals are counted, all of them attention to the work
    /// itself: operator answers (decisions not answered by the supervisor),
    /// hand landings, withdrawals, and hand commits. Operator bookkeeping
    /// (`forge initiative set`, `forge project set`, `forge gc`, renames)
    /// is not among them by construction: none of those verbs writes a
    /// decision, a hand landing, a withdrawal or a base-branch commit, so
    /// nothing needs excluding. `forge task set` is the deliberate
    /// exception: raising a stuck task's own budget or turn cap in place
    /// is exactly the kind of friction this signal exists to surface, so
    /// it writes a decision (see `Store::set_task_limits`) and is counted
    /// here like any other operator answer.
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
    /// (role, provider, model, kind), role being the attempt's step or the
    /// job step's action, and kind (`"attempt"`/`"job_step"`) keeping a
    /// directive step's own provider, model and cost (docs/JOBS.md,
    /// "Steps") from merging into a build task's rows of the same role. For
    /// the `code` role's `"attempt"` rows only, also the landed and
    /// broke-base counts (see `WorkflowStat::broke_base`); a `"job_step"`
    /// row is never tied to a landing, and has no turns or success/failure
    /// of its own, so `landed`/`broke_base` are `None` and
    /// `succeeded`/`mean_turns` are 0 there. `investigate` and `interview`
    /// are read-only directives whose job is to ask when the record does
    /// not settle it; an attempt of either that ended `needs_input` with a
    /// plain question counts as a success here (see `forge stats
    /// --by-role`'s footnote).
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
                    kind: "attempt".to_string(),
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
            let mut stats = rows.collect::<rusqlite::Result<Vec<RoleStat>>>()?;

            let mut job_stmt = c.prepare(
                "SELECT js.action AS role, js.provider AS provider, js.model AS model,
                    COUNT(*) AS attempts,
                    COALESCE(AVG(js.cost_usd), 0) AS mean_cost_usd,
                    AVG((COALESCE(js.finished_at, js.started_at) - js.started_at) * 1000) AS mean_ms
                 FROM job_steps js
                 WHERE js.kind = 'directive'
                 GROUP BY js.action, js.provider, js.model
                 ORDER BY js.action, js.provider, js.model",
            )?;
            let job_rows = job_stmt.query_map([], |r| {
                Ok(RoleStat {
                    role: r.get("role")?,
                    provider: r.get("provider")?,
                    model: r.get("model")?,
                    kind: "job_step".to_string(),
                    attempts: r.get("attempts")?,
                    succeeded: 0,
                    mean_turns: 0.0,
                    mean_cost_usd: r.get("mean_cost_usd")?,
                    mean_ms: r.get("mean_ms")?,
                    landed: None,
                    broke_base: None,
                    repair_cost: None,
                    added_lines: None,
                    churned_lines: None,
                })
            })?;
            stats.extend(job_rows.collect::<rusqlite::Result<Vec<RoleStat>>>()?);
            stats
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
                let Some(r) = stats.iter_mut().find(|r| {
                    r.kind == "attempt"
                        && r.role == "code"
                        && r.provider == provider
                        && r.model == model
                }) else {
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

    /// `forge stats --factors` (docs/ECONOMIST.md, piece 3): over this
    /// scope's landed and failed tasks — finished at or after `since`
    /// when given, every one of them otherwise — one row per level of
    /// every factor (`FactorLevelStat`): `"provider:<role>"` for each
    /// role in `ROLES` a task ran, `"workflow"`, and `"size"`
    /// (`size_class`). `tasks`/`landed`/`rate` and `mean_true_cost_usd`
    /// are read off each level's own tasks; `effect`/`effect_se` come
    /// from one joint main-effects fit of `ln(true cost)` over every
    /// landed task in scope (`fit_main_effects`), a task's true cost
    /// being its attempts' cost plus its cached repair cost (floored at
    /// a cent so the log is always defined). A role that never ran on a
    /// task leaves that task out of that role's factor entirely, both
    /// for the level counts and for the fit's dummy columns.
    pub fn factor_stats(
        &self,
        scope: &StatsFilter,
        since: Option<i64>,
    ) -> Result<Vec<FactorLevelStat>> {
        struct TaskFacts {
            id: i64,
            workflow: String,
            size: &'static str,
            landed: bool,
        }
        let c = self.lock();
        let tasks: Vec<TaskFacts> = {
            let mut stmt = c.prepare(
                "SELECT id, workflow, shape_text_len, shape_path_tokens, landed_sha FROM tasks
                 WHERE state IN ('succeeded','failed','blocked','unverified') AND started_at IS NOT NULL
                   AND (?1 IS NULL OR project = ?1) AND (?2 IS NULL OR initiative = ?2)
                   AND (?3 IS NULL OR finished_at >= ?3)
                 ORDER BY id",
            )?;
            let rows = stmt.query_map(params![scope.project, scope.initiative, since], |r| {
                let text_len: i64 = r.get("shape_text_len")?;
                let path_tokens: i64 = r.get("shape_path_tokens")?;
                let landed_sha: String = r.get("landed_sha")?;
                Ok(TaskFacts {
                    id: r.get("id")?,
                    workflow: r.get("workflow")?,
                    size: size_class(text_len, path_tokens),
                    landed: !landed_sha.is_empty(),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        // Each task's first attempt of each role it ran, in scope: the
        // provider that role ran under (`attempts.provider`, not
        // `Task::routing`, since it is populated for every task rather
        // than only those written after the routing record existed).
        let mut role_providers: BTreeMap<i64, Vec<(String, String)>> = BTreeMap::new();
        {
            // `ROLES` is a fixed, small Rust constant, not user input,
            // so it is safe to splice straight into the `IN` list rather
            // than bind it: a param vector here would have to mix
            // `Option<String>`/`Option<i64>` with five more strings.
            let roles_sql = ROLES
                .iter()
                .map(|r| format!("'{r}'"))
                .collect::<Vec<_>>()
                .join(",");
            let mut stmt = c.prepare(&format!(
                "SELECT a.task_id AS task_id, a.step AS step, a.provider AS provider
                 FROM attempts a JOIN tasks t ON t.id = a.task_id
                 WHERE t.state IN ('succeeded','failed','blocked','unverified') AND t.started_at IS NOT NULL
                   AND (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.initiative = ?2)
                   AND (?3 IS NULL OR t.finished_at >= ?3)
                   AND a.step IN ({roles_sql})
                   AND a.attempt_no = (SELECT MIN(a2.attempt_no) FROM attempts a2
                                        WHERE a2.task_id = a.task_id AND a2.step = a.step)"
            ))?;
            let rows = stmt.query_map(params![scope.project, scope.initiative, since], |r| {
                Ok((
                    r.get::<_, i64>("task_id")?,
                    r.get::<_, String>("step")?,
                    r.get::<_, String>("provider")?,
                ))
            })?;
            for row in rows {
                let (task_id, role, provider) = row?;
                role_providers
                    .entry(task_id)
                    .or_default()
                    .push((role, provider));
            }
        }

        // True cost per landed task: attempt cost plus the cached
        // repair cost (see `WorkflowStat::repair_cost`), 0 when neither
        // has run yet.
        let mut true_cost: BTreeMap<i64, f64> = BTreeMap::new();
        for t in tasks.iter().filter(|t| t.landed) {
            let cost: f64 = c.query_row(
                "SELECT COALESCE(SUM(cost_usd), 0) FROM attempts WHERE task_id=?1",
                params![t.id],
                |r| r.get(0),
            )?;
            let repair = repair_cost_cache_query(&c, t.id)?
                .map(|(v, _)| v)
                .unwrap_or(0.0);
            true_cost.insert(t.id, cost + repair);
        }
        drop(c);

        // Level counts (tasks/landed/true costs), and the fit's rows,
        // built from the same task facts.
        let mut groups: BTreeMap<(String, String), (i64, i64, Vec<f64>)> = BTreeMap::new();
        let bump = |groups: &mut BTreeMap<(String, String), (i64, i64, Vec<f64>)>,
                    key: (String, String),
                    landed: bool,
                    cost: Option<f64>| {
            let e = groups.entry(key).or_insert((0, 0, Vec::new()));
            e.0 += 1;
            if landed {
                e.1 += 1;
                if let Some(c) = cost {
                    e.2.push(c);
                }
            }
        };
        let mut fit_rows: Vec<FitRow> = Vec::new();
        for t in &tasks {
            let cost = true_cost.get(&t.id).copied();
            bump(
                &mut groups,
                ("workflow".to_string(), t.workflow.clone()),
                t.landed,
                cost,
            );
            bump(
                &mut groups,
                ("size".to_string(), t.size.to_string()),
                t.landed,
                cost,
            );
            let roles = role_providers.get(&t.id);
            if let Some(roles) = roles {
                for (role, provider) in roles {
                    bump(
                        &mut groups,
                        (format!("provider:{role}"), provider.clone()),
                        t.landed,
                        cost,
                    );
                }
            }
            if t.landed
                && let Some(cost) = cost
            {
                let mut levels = vec![
                    ("workflow".to_string(), t.workflow.clone()),
                    ("size".to_string(), t.size.to_string()),
                ];
                if let Some(roles) = roles {
                    for (role, provider) in roles {
                        levels.push((format!("provider:{role}"), provider.clone()));
                    }
                }
                fit_rows.push(FitRow {
                    levels,
                    log_cost: cost.max(0.01).ln(),
                });
            }
        }

        // Reference level per factor: whichever level has the most
        // landed tasks carrying it, ties broken alphabetically; a
        // factor stuck at one level (or none) among the landed tasks
        // that carry it is left out of the fit entirely (see
        // `fit_main_effects`).
        let mut landed_counts: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
        for row in &fit_rows {
            for (f, l) in &row.levels {
                *landed_counts
                    .entry(f.clone())
                    .or_default()
                    .entry(l.clone())
                    .or_insert(0) += 1;
            }
        }
        let mut reference: BTreeMap<String, String> = BTreeMap::new();
        for (f, levels) in &landed_counts {
            if levels.len() < 2 {
                continue;
            }
            let mut best: Option<(&String, i64)> = None;
            for (l, &n) in levels {
                if best.is_none_or(|(_, bn)| n > bn) {
                    best = Some((l, n));
                }
            }
            if let Some((l, _)) = best {
                reference.insert(f.clone(), l.clone());
            }
        }

        let effects = fit_main_effects(&fit_rows, &reference);
        let mut out: Vec<FactorLevelStat> = groups
            .into_iter()
            .map(|((factor, level), (tasks_n, landed_n, costs))| {
                let (rate_lo, rate_hi) =
                    crate::profile::wilson(landed_n as usize, tasks_n as usize);
                let rate = if tasks_n > 0 {
                    landed_n as f64 / tasks_n as f64
                } else {
                    0.0
                };
                let mean_true_cost_usd =
                    (!costs.is_empty()).then(|| costs.iter().sum::<f64>() / costs.len() as f64);
                let is_reference = reference.get(&factor) == Some(&level);
                let (effect, effect_se) = effects
                    .get(&(factor.clone(), level.clone()))
                    .map(|&(e, se)| (Some(e), Some(se)))
                    .unwrap_or((None, None));
                FactorLevelStat {
                    factor,
                    level,
                    tasks: tasks_n,
                    landed: landed_n,
                    rate,
                    rate_lo,
                    rate_hi,
                    mean_true_cost_usd,
                    is_reference,
                    effect,
                    effect_se,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            a.factor.cmp(&b.factor).then_with(|| {
                if a.factor == "size" {
                    size_rank(&a.level).cmp(&size_rank(&b.level))
                } else {
                    a.level.cmp(&b.level)
                }
            })
        });
        Ok(out)
    }
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
