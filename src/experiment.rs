//! Piece 4 of the economist: randomized assignment within declared
//! bounds, and the weekly rebalance (docs/ECONOMIST.md, "What is
//! built"). Two things live here:
//!
//! - `load`/`draw_level`: `experiment.toml`, beside the operator's
//!   workflow catalog (`workflows::catalog_dir`), declares each factor's
//!   levels with weights; `queue::enqueue` draws a level per factor for a
//!   task that pins neither its own provider nor a role the task's
//!   project already pins, and the draw is recorded on `Task::explore`
//!   the same way `[measure] explore` already is (piece 2's `"experiment"`
//!   source, `ctx::resolve_provider_routed`).
//! - `rebalance`: the arithmetic `forge economist rebalance` runs weekly
//!   (`.forge/workflows/economist-weekly.toml`), reading `Store::factor_stats`
//!   (the same numbers `forge stats --factors --json` prints) and shifting
//!   `experiment.toml`'s weights toward the cheaper levels, in proportion
//!   to the effect and its confidence, never below the floor.

use crate::store::FactorLevelStat;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// No declared level may fall below this share of its factor's weight
/// unless the operator's `experiment.toml` says otherwise, so the
/// experiment never starves a level to where it can no longer be
/// measured (docs/ECONOMIST.md, "What is built").
pub const DEFAULT_FLOOR: f64 = 0.1;

/// `experiment.toml`'s own shape, parsed as written: `floor` (optional,
/// `DEFAULT_FLOOR` when absent) and `[factors.<role>]`, each key a level
/// name (typically a provider) and value its raw weight.
#[derive(Debug, Clone, Default, Deserialize)]
struct Raw {
    floor: Option<f64>,
    #[serde(default)]
    factors: BTreeMap<String, BTreeMap<String, f64>>,
}

/// The operator's declared experiment: the floor every level must clear,
/// and per factor (a role in `store::ROLES`) the weight to draw each
/// level at, already normalized to sum to 1.0.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExperimentFile {
    pub floor: f64,
    pub factors: BTreeMap<String, BTreeMap<String, f64>>,
}

/// `experiment.toml` under `catalog_dir` (`workflows::catalog_dir`), or
/// `None` when the file does not exist — no experiment declared, every
/// role resolves through the usual layers (docs/ECONOMIST.md, "The
/// routing record"). Every factor name must be a role `store::ROLES`
/// knows, every weight positive, and after normalizing each factor's
/// weights to sum to 1.0, no level may fall under `floor` — a level an
/// operator declared under the floor is a config mistake, refused at
/// load rather than silently starved of draws.
pub fn load(catalog_dir: &Path) -> Result<Option<ExperimentFile>> {
    let path = catalog_dir.join("experiment.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context(format!("reading {}", path.display())),
    };
    let raw: Raw = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let floor = raw.floor.unwrap_or(DEFAULT_FLOOR);
    if !(0.0..0.5).contains(&floor) {
        bail!(
            "{}: floor must be between 0 and 0.5, got {floor}",
            path.display()
        );
    }
    let mut factors = BTreeMap::new();
    for (factor, levels) in raw.factors {
        let normalized = validate_factor(&path, floor, &factor, levels)?;
        factors.insert(factor, normalized);
    }
    Ok(Some(ExperimentFile { floor, factors }))
}

/// One factor's declared weights, checked the way `load` checks every
/// `[factors.*]` table in `experiment.toml`: the role must be one
/// `store::ROLES` knows, every weight positive, and once normalized to
/// sum to 1.0, no level under `floor`. Shared by `load` (every factor in
/// the file) and `forge experiment set` (the one factor an operator sets
/// by hand, applying a rebalance's held proposal) so a weight set by hand
/// can never violate what a weight declared in the file could not.
fn validate_factor(
    path: &Path,
    floor: f64,
    factor: &str,
    levels: BTreeMap<String, f64>,
) -> Result<BTreeMap<String, f64>> {
    if factor == "map" || factor == "tools" || factor == "continuation" {
        let allowed = match factor {
            "map" => ["spans", "names"],
            "tools" => ["outline", "plain"],
            _ => ["resume", "fresh"],
        };
        for level in levels.keys() {
            if !allowed.contains(&level.as_str()) {
                bail!(
                    "{}: [factors.{factor}] level {level:?} is not one of {} (docs/CONTEXT.md)",
                    path.display(),
                    allowed.join(", ")
                );
            }
        }
    } else if !crate::store::ROLES.contains(&factor) {
        bail!(
            "{}: [factors.{factor}] names an unknown role; see `forge stats --by-role` for what runs",
            path.display()
        );
    }
    if levels.is_empty() {
        bail!("{}: [factors.{factor}] declares no levels", path.display());
    }
    if levels.values().any(|&w| w <= 0.0) {
        bail!(
            "{}: [factors.{factor}] every level's weight must be positive",
            path.display()
        );
    }
    let sum: f64 = levels.values().sum();
    let normalized: BTreeMap<String, f64> = levels.into_iter().map(|(l, w)| (l, w / sum)).collect();
    if let Some((level, w)) = normalized.iter().find(|(_, w)| **w < floor - 1e-9) {
        bail!(
            "{}: [factors.{factor}] level {level:?} is {w:.3} of the total, under the floor ({floor:.3})",
            path.display()
        );
    }
    Ok(normalized)
}

/// Sets one factor's weights directly (`forge experiment set`), the same
/// validation `load` applies to every factor in the file
/// (`validate_factor`), then returns the whole `ExperimentFile` with that
/// factor replaced (or added) — ready for `save`. Used to apply a
/// rebalance's proposed weights for a factor `rebalance` held back
/// because a level's effect crossed the threshold (docs/ECONOMIST.md, "a
/// large move is asked about before it compounds"), since a held factor's
/// proposal is never written by `rebalance`/`save` itself.
pub fn set_factor(
    catalog_dir: &Path,
    current: Option<ExperimentFile>,
    factor: &str,
    levels: BTreeMap<String, f64>,
) -> Result<ExperimentFile> {
    let path = catalog_dir.join("experiment.toml");
    let mut exp = current.unwrap_or(ExperimentFile {
        floor: DEFAULT_FLOOR,
        factors: BTreeMap::new(),
    });
    let normalized = validate_factor(&path, exp.floor, factor, levels)?;
    exp.factors.insert(factor.to_string(), normalized);
    Ok(exp)
}

/// Writes `exp` to `experiment.toml` under `catalog_dir`, one `[factors.*]`
/// table per factor, levels in a stable (alphabetical) order so a diff
/// shows only what actually moved.
pub fn save(catalog_dir: &Path, exp: &ExperimentFile) -> Result<()> {
    let mut out = format!("floor = {:.3}\n", exp.floor);
    for (factor, levels) in &exp.factors {
        out.push_str(&format!("\n[factors.{factor}]\n"));
        for (level, w) in levels {
            out.push_str(&format!("{level} = {w:.4}\n"));
        }
    }
    std::fs::write(catalog_dir.join("experiment.toml"), out)
        .with_context(|| format!("writing {}", catalog_dir.join("experiment.toml").display()))
}

/// A splitmix64-style finalizer over a task id and a factor name: the
/// same technique `queue::journal_control_draw` uses for its own single
/// draw, extended so every factor of one task draws independently of
/// every other (docs/ECONOMIST.md: "the assignment is independent per
/// factor and per task") rather than testing one shared draw against
/// several thresholds.
fn draw01(id: i64, factor: &str) -> f64 {
    let mut x = id as u64;
    for b in factor.bytes() {
        x = x.wrapping_mul(0x0000_0100_0000_01b3).wrapping_add(b as u64);
    }
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    (x % 1_000_000) as f64 / 1_000_000.0
}

/// Draws one level of `factor` for task `id`, by `weights` — a pure
/// function of `id` and `factor`, reproducible without persisting the
/// draw separately from the id it came from (the same property
/// `journal_control_draw` and `assign_explore` have). `None` when
/// `weights` is empty. `weights` need not already sum to 1.0: the draw
/// walks the cumulative sum and falls back to the last level, so any
/// positive scale works.
pub fn draw_level(id: i64, factor: &str, weights: &BTreeMap<String, f64>) -> Option<String> {
    if weights.is_empty() {
        return None;
    }
    let total: f64 = weights.values().sum();
    if total <= 0.0 {
        return None;
    }
    let draw = draw01(id, factor) * total;
    let mut cum = 0.0;
    for (level, w) in weights {
        cum += w;
        if draw < cum {
            return Some(level.clone());
        }
    }
    weights.keys().next_back().cloned()
}

/// Extends `explore` (already carrying any `[measure] explore` draws)
/// with a level per experiment factor whose role is not already claimed
/// there and that `project_roles` does not pin — a project's own pin
/// always wins over the experiment (docs/ECONOMIST.md: randomized
/// assignment happens only "within declared bounds", never against a
/// pin). No-op when `exp` is `None` (no `experiment.toml`).
pub fn extend_explore(
    explore: &mut BTreeMap<String, String>,
    id: i64,
    project_roles: &BTreeMap<String, String>,
    exp: Option<&ExperimentFile>,
) {
    let Some(exp) = exp else { return };
    for (factor, weights) in &exp.factors {
        if explore.contains_key(factor) || project_roles.contains_key(factor) {
            continue;
        }
        if let Some(level) = draw_level(id, factor, weights) {
            explore.insert(factor.clone(), level);
        }
    }
}

/// Normalizes `weights` to sum to 1.0, then raises any level under
/// `floor` up to it and shrinks the rest proportionally so the factor
/// still sums to 1.0 (docs/ECONOMIST.md: "a floor... no weight goes
/// below"). Water-filling: fixing a violator to the floor can push a
/// level that was fine into violation once the remainder is rescaled, so
/// it repeats until nothing more needs raising — at most one factor
/// fixed per pass, so it terminates within `weights.len()` passes.
/// `floor` itself is capped at `1/n` first: a floor no even split could
/// clear is not honoured, it is impossible.
pub fn apply_floor(weights: &BTreeMap<String, f64>, floor: f64) -> BTreeMap<String, f64> {
    let n = weights.len();
    if n == 0 {
        return BTreeMap::new();
    }
    let floor = floor.max(0.0).min(1.0 / n as f64);
    let sum: f64 = weights.values().sum();
    let mut out: BTreeMap<String, f64> = if sum > 0.0 {
        weights
            .iter()
            .map(|(k, &v)| (k.clone(), v.max(0.0) / sum))
            .collect()
    } else {
        weights
            .keys()
            .map(|k| (k.clone(), 1.0 / n as f64))
            .collect()
    };
    let mut fixed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    loop {
        let below: Vec<String> = out
            .iter()
            .filter(|(k, v)| !fixed.contains(*k) && **v < floor - 1e-12)
            .map(|(k, _)| k.clone())
            .collect();
        if below.is_empty() {
            break;
        }
        for k in &below {
            out.insert(k.clone(), floor);
            fixed.insert(k.clone());
        }
        let remaining = (1.0 - floor * fixed.len() as f64).max(0.0);
        let free_sum: f64 = out
            .iter()
            .filter(|(k, _)| !fixed.contains(*k))
            .map(|(_, &v)| v)
            .sum();
        let free_n = n - fixed.len();
        for (k, v) in out.iter_mut() {
            if fixed.contains(k) {
                continue;
            }
            *v = if free_sum > 0.0 {
                *v * remaining / free_sum
            } else if free_n > 0 {
                remaining / free_n as f64
            } else {
                0.0
            };
        }
    }
    out
}

/// One level whose effect crossed the rebalance's threshold — files a
/// question through the human rung before the week's shift compounds
/// (docs/ECONOMIST.md, "What is built").
#[derive(Debug, Clone, PartialEq)]
pub struct LargeMove {
    pub factor: String,
    pub level: String,
    pub effect: f64,
    pub effect_se: Option<f64>,
}

/// One factor's weights before and after `rebalance`, for the commit
/// message and the dry-run print; only factors whose weights actually
/// moved are recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct FactorShift {
    pub factor: String,
    pub before: BTreeMap<String, f64>,
    pub after: BTreeMap<String, f64>,
}

pub struct RebalanceResult {
    pub factors: BTreeMap<String, BTreeMap<String, f64>>,
    pub shifts: Vec<FactorShift>,
    pub large_moves: Vec<LargeMove>,
    /// Factors `rebalance` left untouched because one of their levels
    /// crossed the threshold: `before` is the current (unchanged) weights,
    /// `after` the proposal `rebalance` computed but did not write — named
    /// in the question so the operator can apply it with `forge experiment
    /// set` (docs/ECONOMIST.md, "a large move is asked about before it
    /// compounds").
    pub held: Vec<FactorShift>,
}

/// The week's step size: level `l`'s weight is multiplied by `1 +
/// LEARNING_RATE * signal(l)` before renormalizing — a fraction of a
/// swing per week, not a jump, so a bad week's noise costs little and a
/// real, repeated difference compounds.
const LEARNING_RATE: f64 = 0.35;
/// A standard error this small or smaller is treated as this small, so a
/// tiny handful of landed tasks under one level (an underpowered `se`)
/// cannot produce an oversized signal.
const MIN_SE: f64 = 0.05;
/// `signal` is a t-stat-like ratio (`-effect / se`), clamped here so one
/// extreme week cannot move a factor's weights by more than
/// `LEARNING_RATE * MAX_SIGNAL` at once.
const MAX_SIGNAL: f64 = 3.0;

/// How strongly one week's numbers push a level's weight: positive when
/// `effect` says this level is cheaper than its factor's reference level
/// and `effect_se` says that's a confident read, negative when costlier,
/// `0.0` when the fit has nothing to say about this level this week (the
/// reference level itself, whose `effect` is always `None`, or a level
/// the fit dropped for too little data).
fn signal(effect: Option<f64>, effect_se: Option<f64>) -> f64 {
    match (effect, effect_se) {
        (Some(e), Some(se)) if se.is_finite() && e.is_finite() => {
            (-e / se.max(MIN_SE)).clamp(-MAX_SIGNAL, MAX_SIGNAL)
        }
        _ => 0.0,
    }
}

/// Shifts `current`'s weights toward the levels with lower true cost per
/// landed task, in proportion to the effect and its confidence (`signal`),
/// renormalizes, then floors (`apply_floor`) so no level goes under
/// `floor`. `stats` is `Store::factor_stats`'s output (the same rows
/// `forge stats --factors --json` prints); only its `"provider:<role>"`
/// rows matter here; `"workflow"` and `"size"` are not factors an
/// experiment draws. A factor in `current` that this window's `stats`
/// never mention (or a level within it) keeps its old weight — nothing
/// to shift toward without data.
///
/// A factor with a level whose `effect` clears `threshold` in magnitude is
/// a large move (docs/ECONOMIST.md, "a large move is asked about before it
/// compounds"): that whole factor is left at its current weights in
/// `factors` — nothing is written for it — and its computed proposal goes
/// to `held` instead of `shifts`, so it never gets committed quietly. A
/// factor with no large move is rebalanced and recorded in `shifts` (when
/// it actually moved) as usual. Every crossing level, whichever factor
/// it's in, is returned in `large_moves` — the human rung's trigger.
pub fn rebalance(
    current: &BTreeMap<String, BTreeMap<String, f64>>,
    floor: f64,
    stats: &[FactorLevelStat],
    threshold: f64,
) -> RebalanceResult {
    let mut by_factor: BTreeMap<&str, BTreeMap<&str, &FactorLevelStat>> = BTreeMap::new();
    for s in stats {
        let Some(role) = s.factor.strip_prefix("provider:") else {
            continue;
        };
        by_factor.entry(role).or_default().insert(&s.level, s);
    }
    let mut factors = BTreeMap::new();
    let mut shifts = Vec::new();
    let mut large_moves = Vec::new();
    let mut held = Vec::new();
    for (factor, weights) in current {
        let levels = by_factor.get(factor.as_str());
        let mut tilted: BTreeMap<String, f64> = BTreeMap::new();
        let mut crossed = Vec::new();
        for (level, &w) in weights {
            let stat = levels.and_then(|m| m.get(level.as_str()));
            let sig = stat.map(|s| signal(s.effect, s.effect_se)).unwrap_or(0.0);
            tilted.insert(level.clone(), (w * (1.0 + LEARNING_RATE * sig)).max(1e-9));
            if let Some(s) = stat
                && let Some(e) = s.effect
                && e.abs() > threshold
            {
                crossed.push(LargeMove {
                    factor: factor.clone(),
                    level: level.clone(),
                    effect: e,
                    effect_se: s.effect_se,
                });
            }
        }
        let proposed = apply_floor(&tilted, floor);
        if crossed.is_empty() {
            if &proposed != weights {
                shifts.push(FactorShift {
                    factor: factor.clone(),
                    before: weights.clone(),
                    after: proposed.clone(),
                });
            }
            factors.insert(factor.clone(), proposed);
        } else {
            large_moves.extend(crossed);
            held.push(FactorShift {
                factor: factor.clone(),
                before: weights.clone(),
                after: proposed,
            });
            factors.insert(factor.clone(), weights.clone());
        }
    }
    RebalanceResult {
        factors,
        shifts,
        large_moves,
        held,
    }
}

/// The question a large move files (docs/ECONOMIST.md, "a large move is
/// asked about before it compounds"): every crossing level and, per held
/// factor, the proposed weights `rebalance` computed but did not write, as
/// the `forge experiment set` invocation that would apply them.
pub fn large_move_message(
    large_moves: &[LargeMove],
    held: &[FactorShift],
    threshold: f64,
) -> String {
    let mut out = format!(
        "{} level(s) crossed the effect threshold; asking the operator\n",
        large_moves.len()
    );
    for m in large_moves {
        out.push_str(&format!(
            "large effect: {}:{} = {:+.2} log$ (se {}), over the {threshold:.2} threshold\n",
            m.factor,
            m.level,
            m.effect,
            m.effect_se.map_or("-".to_string(), |se| format!("{se:.2}")),
        ));
    }
    for h in held {
        let levels: Vec<String> = h
            .after
            .iter()
            .map(|(level, w)| format!("{level}={w:.4}"))
            .collect();
        out.push_str(&format!(
            "proposed: forge experiment set {} {}\n",
            h.factor,
            levels.join(" ")
        ));
    }
    out
}

/// The git commit message `forge economist rebalance` writes, naming
/// every factor and level that actually moved (docs/ECONOMIST.md, "What
/// is built": "commits it in the catalog's git with a message naming the
/// effects").
pub fn commit_message(shifts: &[FactorShift]) -> String {
    if shifts.is_empty() {
        return "economist: rebalance, no factor's weights moved".to_string();
    }
    let parts: Vec<String> = shifts
        .iter()
        .map(|s| {
            let levels: Vec<String> = s
                .after
                .iter()
                .filter(|(level, w)| {
                    (**w - s.before.get(level.as_str()).copied().unwrap_or(0.0)).abs() > 1e-6
                })
                .map(|(level, &w)| {
                    let before = s.before.get(level.as_str()).copied().unwrap_or(0.0);
                    format!("{level} {before:.2}->{w:.2}")
                })
                .collect();
            format!("{}: {}", s.factor, levels.join(", "))
        })
        .collect();
    format!("economist: rebalance ({})", parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn stat(factor: &str, level: &str, effect: Option<f64>, se: Option<f64>) -> FactorLevelStat {
        FactorLevelStat {
            factor: factor.to_string(),
            level: level.to_string(),
            tasks: 20,
            landed: 15,
            rate: 0.75,
            rate_lo: 0.5,
            rate_hi: 0.9,
            mean_true_cost_usd: Some(2.0),
            is_reference: effect.is_none(),
            effect,
            effect_se: se,
            mean_first_edit_call: None,
            mean_calls_per_turn: None,
            mean_grep_then_ranged_read_chains: None,
            mean_unedited_read_chars: None,
            mean_turns_before_first_edit: None,
            mean_outline_calls: None,
            mean_def_calls: None,
        }
    }

    #[test]
    fn tools_factor_validates_saves_and_preserves_the_draw() {
        let dir = tempfile::tempdir().unwrap();
        let exp = set_factor(
            dir.path(),
            None,
            "tools",
            w(&[("outline", 1.0), ("plain", 1.0)]),
        )
        .unwrap();
        let mut draw = BTreeMap::new();
        extend_explore(&mut draw, 42, &BTreeMap::new(), Some(&exp));
        assert!(matches!(draw["tools"].as_str(), "outline" | "plain"));
        let original = draw.clone();
        extend_explore(&mut draw, 43, &BTreeMap::new(), Some(&exp));
        assert_eq!(draw, original);
        save(dir.path(), &exp).unwrap();
        assert_eq!(
            load(dir.path()).unwrap().unwrap().factors["tools"],
            w(&[("outline", 0.5), ("plain", 0.5)])
        );
        assert!(set_factor(dir.path(), None, "tools", w(&[("unknown", 1.0)])).is_err());
    }

    // --- load: floor and role validation --------------------------------

    #[test]
    fn load_returns_none_when_experiment_toml_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn load_refuses_an_unknown_role_and_a_sub_floor_level() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("experiment.toml"),
            "[factors.review]\nanthropic = 0.95\nopenai = 0.05\n",
        )
        .unwrap();
        let err = load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("floor"), "{err}");

        std::fs::write(
            dir.path().join("experiment.toml"),
            "[factors.snacks]\nanthropic = 1.0\n",
        )
        .unwrap();
        let err = load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("unknown role"), "{err}");
    }

    #[test]
    fn load_normalizes_weights_that_do_not_sum_to_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("experiment.toml"),
            "[factors.review]\nanthropic = 7\nopenai = 3\n",
        )
        .unwrap();
        let exp = load(dir.path()).unwrap().unwrap();
        let levels = &exp.factors["review"];
        assert!((levels["anthropic"] - 0.7).abs() < 1e-9);
        assert!((levels["openai"] - 0.3).abs() < 1e-9);
    }

    // --- the draw: weights respected, floor honoured ---------------------

    #[test]
    fn draw_level_respects_weights_over_many_draws() {
        let weights = w(&[("anthropic", 0.7), ("openai", 0.3)]);
        let mut counts: BTreeMap<&str, i64> = BTreeMap::new();
        let n = 20_000;
        for id in 0..n {
            let level = draw_level(id, "review", &weights).unwrap();
            *counts
                .entry(if level == "anthropic" {
                    "anthropic"
                } else {
                    "openai"
                })
                .or_insert(0) += 1;
        }
        let share_anthropic = counts["anthropic"] as f64 / n as f64;
        assert!(
            (share_anthropic - 0.7).abs() < 0.02,
            "share {share_anthropic}"
        );
    }

    #[test]
    fn draw_level_draws_each_factor_independently_of_the_others() {
        // Two factors at the same 50/50 weights: if the draw reused one
        // shared random number per task (as `journal_control_draw` does
        // for its own single factor), both factors would always agree.
        // Keying the draw by id *and* factor name breaks that coupling.
        let weights = w(&[("a", 0.5), ("b", 0.5)]);
        let mut agree = 0;
        let n = 2_000;
        for id in 0..n {
            let x = draw_level(id, "code", &weights).unwrap();
            let y = draw_level(id, "review", &weights).unwrap();
            if x == y {
                agree += 1;
            }
        }
        let share = agree as f64 / n as f64;
        assert!((share - 0.5).abs() < 0.1, "share {share}");
    }

    #[test]
    fn draw_level_is_a_pure_function_of_id_and_factor() {
        let weights = w(&[("anthropic", 0.6), ("openai", 0.4)]);
        for id in [1, 2, 3, 42, 1000] {
            let a = draw_level(id, "code", &weights);
            let b = draw_level(id, "code", &weights);
            assert_eq!(a, b, "id {id}");
        }
    }

    #[test]
    fn apply_floor_raises_a_level_under_it_and_shrinks_the_rest() {
        let weights = w(&[("anthropic", 0.98), ("openai", 0.02)]);
        let out = apply_floor(&weights, 0.1);
        assert!((out["openai"] - 0.1).abs() < 1e-9, "{out:?}");
        assert!((out["anthropic"] - 0.9).abs() < 1e-9, "{out:?}");
        let sum: f64 = out.values().sum();
        assert!((sum - 1.0).abs() < 1e-9);
    }

    #[test]
    fn apply_floor_cascades_when_raising_one_level_would_starve_another() {
        // Three levels, two already near the floor: raising the lowest
        // to 0.2 and rescaling the rest would push the second under 0.2
        // too, so it must also be raised, in a second pass.
        let weights = w(&[("a", 0.65), ("b", 0.22), ("c", 0.13)]);
        let out = apply_floor(&weights, 0.2);
        assert!(out.values().all(|&v| v >= 0.2 - 1e-9), "{out:?}");
        let sum: f64 = out.values().sum();
        assert!((sum - 1.0).abs() < 1e-9);
    }

    #[test]
    fn draw_level_never_draws_a_level_the_floor_would_forbid() {
        // A weight far under a strict floor should still surface with
        // roughly the floor's own share once the config layer applies
        // it — exercised here directly against `apply_floor` feeding
        // `draw_level`, the shape `load` enforces for a real file.
        let raw = w(&[("anthropic", 0.99), ("openai", 0.01)]);
        let floored = apply_floor(&raw, 0.1);
        let mut hits = 0;
        let n = 20_000;
        for id in 0..n {
            if draw_level(id, "code", &floored).unwrap() == "openai" {
                hits += 1;
            }
        }
        let share = hits as f64 / n as f64;
        assert!((share - 0.1).abs() < 0.02, "share {share}");
    }

    // --- the rebalance arithmetic -----------------------------------------

    #[test]
    fn rebalance_shifts_weight_toward_the_cheaper_confident_level() {
        let mut current = BTreeMap::new();
        current.insert(
            "review".to_string(),
            w(&[("anthropic", 0.5), ("openai", 0.5)]),
        );
        let stats = vec![
            stat("provider:review", "anthropic", None, None), // reference
            stat("provider:review", "openai", Some(-0.8), Some(0.1)), // cheaper, confident
        ];
        let result = rebalance(&current, DEFAULT_FLOOR, &stats, 10.0);
        let after = &result.factors["review"];
        assert!(after["openai"] > 0.5, "{after:?}");
        assert!(after["anthropic"] < 0.5, "{after:?}");
        assert_eq!(result.shifts.len(), 1);
        assert!(result.large_moves.is_empty());
    }

    #[test]
    fn rebalance_shifts_weight_away_from_a_confident_costlier_level() {
        let mut current = BTreeMap::new();
        current.insert(
            "review".to_string(),
            w(&[("anthropic", 0.5), ("openai", 0.5)]),
        );
        let stats = vec![
            stat("provider:review", "anthropic", None, None),
            stat("provider:review", "openai", Some(0.8), Some(0.1)), // costlier, confident
        ];
        let result = rebalance(&current, DEFAULT_FLOOR, &stats, 10.0);
        let after = &result.factors["review"];
        assert!(after["openai"] < 0.5, "{after:?}");
    }

    #[test]
    fn rebalance_never_pushes_a_weight_under_the_floor() {
        let mut current = BTreeMap::new();
        current.insert(
            "review".to_string(),
            w(&[("anthropic", 0.85), ("openai", 0.15)]),
        );
        let stats = vec![
            stat("provider:review", "anthropic", None, None),
            // A huge, confident effect against openai: an unfloored
            // multiplicative tilt would drive it toward zero.
            stat("provider:review", "openai", Some(3.0), Some(0.05)),
        ];
        let result = rebalance(&current, 0.1, &stats, 10.0);
        assert!(result.factors["review"]["openai"] >= 0.1 - 1e-9);
    }

    #[test]
    fn rebalance_leaves_a_factor_with_no_stats_this_window_untouched() {
        let mut current = BTreeMap::new();
        current.insert(
            "plan".to_string(),
            w(&[("anthropic", 0.6), ("openai", 0.4)]),
        );
        let result = rebalance(&current, DEFAULT_FLOOR, &[], 10.0);
        assert_eq!(result.factors["plan"], current["plan"]);
        assert!(result.shifts.is_empty());
    }

    #[test]
    fn rebalance_flags_a_level_whose_effect_crosses_the_threshold() {
        let mut current = BTreeMap::new();
        current.insert(
            "review".to_string(),
            w(&[("anthropic", 0.5), ("openai", 0.5)]),
        );
        let stats = vec![
            stat("provider:review", "anthropic", None, None),
            stat("provider:review", "openai", Some(1.4), Some(0.2)),
        ];
        let result = rebalance(&current, DEFAULT_FLOOR, &stats, 1.0);
        assert_eq!(result.large_moves.len(), 1);
        assert_eq!(result.large_moves[0].level, "openai");
    }

    #[test]
    fn rebalance_holds_an_over_threshold_factor_but_moves_its_sibling() {
        let mut current = BTreeMap::new();
        current.insert(
            "review".to_string(),
            w(&[("anthropic", 0.5), ("openai", 0.5)]),
        );
        current.insert(
            "code".to_string(),
            w(&[("anthropic", 0.5), ("openai", 0.5)]),
        );
        let stats = vec![
            stat("provider:review", "anthropic", None, None),
            // Crosses the 1.0 threshold: "review" must be held.
            stat("provider:review", "openai", Some(1.4), Some(0.2)),
            stat("provider:code", "anthropic", None, None),
            // Cheaper, confident, but under the threshold: "code" moves.
            stat("provider:code", "openai", Some(-0.8), Some(0.1)),
        ];
        let result = rebalance(&current, DEFAULT_FLOOR, &stats, 1.0);

        assert_eq!(result.factors["review"], current["review"], "held factor");
        assert!(
            result.shifts.iter().all(|s| s.factor != "review"),
            "{:?}",
            result.shifts
        );
        assert_eq!(result.held.len(), 1);
        assert_eq!(result.held[0].factor, "review");
        assert_eq!(result.held[0].before, current["review"]);
        assert_ne!(result.held[0].after, current["review"], "{:?}", result.held);

        assert!(
            result.factors["code"]["openai"] > 0.5,
            "{:?}",
            result.factors
        );
        assert!(result.shifts.iter().any(|s| s.factor == "code"));
    }

    #[test]
    fn large_move_message_names_the_proposed_weights_for_a_held_factor() {
        let large_moves = vec![LargeMove {
            factor: "review".to_string(),
            level: "openai".to_string(),
            effect: 1.4,
            effect_se: Some(0.2),
        }];
        let held = vec![FactorShift {
            factor: "review".to_string(),
            before: w(&[("anthropic", 0.5), ("openai", 0.5)]),
            after: w(&[("anthropic", 0.35), ("openai", 0.65)]),
        }];
        let msg = large_move_message(&large_moves, &held, 1.0);
        assert!(msg.contains("review:openai"), "{msg}");
        assert!(msg.contains("forge experiment set review"), "{msg}");
        assert!(msg.contains("openai=0.6500"), "{msg}");
        assert!(msg.contains("anthropic=0.3500"), "{msg}");
    }

    #[test]
    fn commit_message_names_the_levels_that_moved() {
        let mut before = BTreeMap::new();
        before.insert("anthropic".to_string(), 0.5);
        before.insert("openai".to_string(), 0.5);
        let mut after = BTreeMap::new();
        after.insert("anthropic".to_string(), 0.4);
        after.insert("openai".to_string(), 0.6);
        let shifts = vec![FactorShift {
            factor: "review".to_string(),
            before,
            after,
        }];
        let msg = commit_message(&shifts);
        assert!(msg.contains("review"), "{msg}");
        assert!(msg.contains("openai 0.50->0.60"), "{msg}");
    }

    #[test]
    fn commit_message_names_no_shift_when_nothing_moved() {
        assert!(commit_message(&[]).contains("no factor"));
    }

    // --- extend_explore: project pins and existing explore draws win ------

    #[test]
    fn extend_explore_skips_a_role_the_project_pins() {
        let mut exp_factors = BTreeMap::new();
        exp_factors.insert("code".to_string(), w(&[("anthropic", 1.0)]));
        let exp = ExperimentFile {
            floor: DEFAULT_FLOOR,
            factors: exp_factors,
        };
        let mut explore = BTreeMap::new();
        let mut project_roles = BTreeMap::new();
        project_roles.insert("code".to_string(), "openai".to_string());
        extend_explore(&mut explore, 1, &project_roles, Some(&exp));
        assert!(explore.is_empty());
    }

    #[test]
    fn extend_explore_never_overrides_an_existing_explore_draw() {
        let mut exp_factors = BTreeMap::new();
        exp_factors.insert("code".to_string(), w(&[("openai", 1.0)]));
        let exp = ExperimentFile {
            floor: DEFAULT_FLOOR,
            factors: exp_factors,
        };
        let mut explore = BTreeMap::new();
        explore.insert("code".to_string(), "anthropic".to_string());
        extend_explore(&mut explore, 1, &BTreeMap::new(), Some(&exp));
        assert_eq!(explore["code"], "anthropic");
    }

    #[test]
    fn extend_explore_draws_an_unpinned_role() {
        let mut exp_factors = BTreeMap::new();
        exp_factors.insert("code".to_string(), w(&[("openai", 1.0)]));
        let exp = ExperimentFile {
            floor: DEFAULT_FLOOR,
            factors: exp_factors,
        };
        let mut explore = BTreeMap::new();
        extend_explore(&mut explore, 1, &BTreeMap::new(), Some(&exp));
        assert_eq!(explore["code"], "openai");
    }
}
