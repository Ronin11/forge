//! `forge stats --factors`: the factor levels, the main-effects fit and
//! the size classes (docs/ECONOMIST.md, piece 3). Split out of stats.rs
//! on 2026-09-22 when the map factor and two exploration measures put
//! that file over the store's 1500-line bound.

use super::stats::repair_cost_cache_query;
use super::*;

/// The roles `Task::routing` (and every `code`/`tests`/`review`/`plan`/
/// `assess` attempt) can name: what `forge stats --factors`'s "provider
/// per role" factor iterates over (see docs/ECONOMIST.md, "The inputs,
/// and where each lives" and "Provider per role"), and the factor names
/// `experiment.toml` may declare (piece 4, `crate::experiment::load`).
pub const ROLES: [&str; 5] = ["code", "tests", "review", "plan", "assess"];

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
    /// Exploration, over this level's tasks' first code attempts: the mean
    /// tool call at which the first edit came (`attempts.first_edit`), and
    /// the mean tool calls per turn. `None` when no attempt recorded them.
    /// What the map factor (docs/CONTEXT.md) is measured by.
    pub mean_first_edit_call: Option<f64>,
    pub mean_calls_per_turn: Option<f64>,
    pub mean_grep_then_ranged_read_chains: Option<f64>,
    pub mean_unedited_read_chars: Option<f64>,
    pub mean_turns_before_first_edit: Option<f64>,
    pub mean_outline_calls: Option<f64>,
    pub mean_def_calls: Option<f64>,
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

impl Store {
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
            /// The `map` factor's level from the task's explore draw, when
            /// one was drawn (docs/CONTEXT.md, the map factor).
            map: Option<String>,
            tools: Option<String>,
        }
        let c = self.lock();
        let tasks: Vec<TaskFacts> = {
            let mut stmt = c.prepare(
                "SELECT id, workflow, shape_text_len, shape_path_tokens, landed_sha, explore_json FROM tasks
                 WHERE state IN ('succeeded','failed','blocked','unverified') AND started_at IS NOT NULL
                   AND (?1 IS NULL OR project = ?1) AND (?2 IS NULL OR initiative = ?2)
                   AND (?3 IS NULL OR finished_at >= ?3)
                 ORDER BY id",
            )?;
            let rows = stmt.query_map(params![scope.project, scope.initiative, since], |r| {
                let text_len: i64 = r.get("shape_text_len")?;
                let path_tokens: i64 = r.get("shape_path_tokens")?;
                let landed_sha: String = r.get("landed_sha")?;
                let explore: String = r
                    .get::<_, Option<String>>("explore_json")?
                    .unwrap_or_default();
                let map = serde_json::from_str::<serde_json::Value>(&explore)
                    .ok()
                    .and_then(|v| v.get("map").and_then(|m| m.as_str()).map(str::to_string));
                let tools = serde_json::from_str::<serde_json::Value>(&explore)
                    .ok()
                    .and_then(|v| v.get("tools").and_then(|m| m.as_str()).map(str::to_string));
                Ok(TaskFacts {
                    id: r.get("id")?,
                    workflow: r.get("workflow")?,
                    size: size_class(text_len, path_tokens),
                    landed: !landed_sha.is_empty(),
                    map,
                    tools,
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
        // Exploration per task, from its code attempts: the first edit's
        // tool call on the first attempt that recorded one, and tool calls
        // per turn over every code attempt.
        let mut explore_facts: BTreeMap<i64, (Option<f64>, Option<f64>)> = BTreeMap::new();
        {
            let mut stmt = c.prepare(
                "SELECT task_id, MIN(first_edit) AS first_edit, SUM(tool_calls) AS calls, SUM(num_turns) AS turns
                 FROM attempts WHERE step = 'code' GROUP BY task_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>("task_id")?,
                    r.get::<_, Option<i64>>("first_edit")?,
                    r.get::<_, Option<i64>>("calls")?,
                    r.get::<_, Option<i64>>("turns")?,
                ))
            })?;
            for row in rows {
                let (task_id, first_edit, calls, turns) = row?;
                let per_turn = match (calls, turns) {
                    (Some(c), Some(t)) if t > 0 => Some(c as f64 / t as f64),
                    _ => None,
                };
                explore_facts.insert(task_id, (first_edit.map(|v| v as f64), per_turn));
            }
        }

        let mut navigation: BTreeMap<i64, Vec<crate::tools::exploration::Measures>> =
            BTreeMap::new();
        {
            let mut stmt = c.prepare("SELECT task_id, outputs_json FROM attempts")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (id, json) = row?;
                if let Ok(outputs) = serde_json::from_str::<crate::audit::Outputs>(&json)
                    && let Some(measures) = outputs.tools.and_then(|t| t.exploration)
                {
                    navigation.entry(id).or_default().push(measures);
                }
            }
        }

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
        #[derive(Default)]
        struct Group {
            tasks: i64,
            landed: i64,
            costs: Vec<f64>,
            first_edits: Vec<f64>,
            calls_per_turn: Vec<f64>,
            grep_then_ranged_read_chains: Vec<f64>,
            unedited_read_chars: Vec<f64>,
            turns_before_first_edit: Vec<f64>,
            outline_calls: Vec<f64>,
            def_calls: Vec<f64>,
        }
        let mut groups: BTreeMap<(String, String), Group> = BTreeMap::new();
        let bump = |groups: &mut BTreeMap<(String, String), Group>,
                    key: (String, String),
                    landed: bool,
                    cost: Option<f64>,
                    explore: Option<&(Option<f64>, Option<f64>)>| {
            let e = groups.entry(key).or_default();
            e.tasks += 1;
            if landed {
                e.landed += 1;
                if let Some(c) = cost {
                    e.costs.push(c);
                }
            }
            if let Some((fe, cpt)) = explore {
                if let Some(v) = fe {
                    e.first_edits.push(*v);
                }
                if let Some(v) = cpt {
                    e.calls_per_turn.push(*v);
                }
            }
        };
        let mut fit_rows: Vec<FitRow> = Vec::new();
        for t in &tasks {
            let cost = true_cost.get(&t.id).copied();
            let ex = explore_facts.get(&t.id);
            bump(
                &mut groups,
                ("workflow".to_string(), t.workflow.clone()),
                t.landed,
                cost,
                ex,
            );
            bump(
                &mut groups,
                ("size".to_string(), t.size.to_string()),
                t.landed,
                cost,
                ex,
            );
            if let Some(map) = &t.map {
                bump(
                    &mut groups,
                    ("map".to_string(), map.clone()),
                    t.landed,
                    cost,
                    ex,
                );
            }
            if let Some(level) = &t.tools {
                let key = ("tools".to_string(), level.clone());
                bump(&mut groups, key.clone(), t.landed, cost, ex);
                let group = groups.get_mut(&key).unwrap();
                for m in navigation.get(&t.id).into_iter().flatten() {
                    group
                        .grep_then_ranged_read_chains
                        .push(m.grep_then_ranged_read_chains as f64);
                    group.unedited_read_chars.push(m.unedited_read_chars as f64);
                    if let Some(v) = m.turns_before_first_edit {
                        group.turns_before_first_edit.push(v as f64);
                    }
                    group.outline_calls.push(m.outline_calls as f64);
                    group.def_calls.push(m.def_calls as f64);
                }
            }
            let roles = role_providers.get(&t.id);
            if let Some(roles) = roles {
                for (role, provider) in roles {
                    bump(
                        &mut groups,
                        (format!("provider:{role}"), provider.clone()),
                        t.landed,
                        cost,
                        ex,
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
                if let Some(map) = &t.map {
                    levels.push(("map".to_string(), map.clone()));
                }
                if let Some(level) = &t.tools {
                    levels.push(("tools".to_string(), level.clone()));
                }
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
            .map(|((factor, level), g)| {
                let (tasks_n, landed_n, costs) = (g.tasks, g.landed, g.costs);
                let mean =
                    |v: &Vec<f64>| (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64);
                let mean_first_edit_call = mean(&g.first_edits);
                let mean_calls_per_turn = mean(&g.calls_per_turn);
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
                    mean_first_edit_call,
                    mean_calls_per_turn,
                    mean_grep_then_ranged_read_chains: mean(&g.grep_then_ranged_read_chains),
                    mean_unedited_read_chars: mean(&g.unedited_read_chars),
                    mean_turns_before_first_edit: mean(&g.turns_before_first_edit),
                    mean_outline_calls: mean(&g.outline_calls),
                    mean_def_calls: mean(&g.def_calls),
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
