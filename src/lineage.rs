//! `forge audit`'s lineage walk: given one lineage's rows (`Store::lineage`'s
//! own output, root first), where its retry chain ends and what became of
//! it. Pure and unit-tested, with no store of its own; `cli::audit` and
//! `doctor::dangling` both feed it from the same tested `Store::lineage`.

use crate::store::{LineageRow, TaskState};
use serde::Serialize;
use std::collections::BTreeMap;

/// What became of a lineage, read from its tip alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Landed,
    InProgress,
    Withdrawn,
    Unverified,
    Question,
    Dangling,
}

impl Outcome {
    pub const ALL: [Outcome; 6] = [
        Outcome::Landed,
        Outcome::InProgress,
        Outcome::Withdrawn,
        Outcome::Unverified,
        Outcome::Question,
        Outcome::Dangling,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Landed => "landed",
            Outcome::InProgress => "in progress",
            Outcome::Withdrawn => "withdrawn",
            Outcome::Unverified => "unverified",
            Outcome::Question => "question",
            Outcome::Dangling => "dangling",
        }
    }
}

/// A blocked reason that is a question for a person, distinct from a
/// workflow request, a suite conflict, a review demotion or a dependency
/// wait (mirrors `view::request_kind`'s "needs input" case; kept local so
/// this pure module names no layer above `store`).
fn is_question(reason: &str) -> bool {
    reason.starts_with("needs input:")
}

/// The task in `rows` that retries `id`, the most recent when more than
/// one does (a review demotion's follow-up beside a hand retry both retry
/// the same parent): the same pick `Store::latest_retry_of` makes.
fn newest_retry_of(id: i64, rows: &[LineageRow]) -> Option<i64> {
    // HOOK: once task 1057's `supersedes` column lands, a task whose
    // `supersedes == Some(id)` is a follow-up too; fold it into this same
    // max-of-candidates pick and nothing else in this file needs to change.
    rows.iter()
        .filter(|r| r.parent == Some(id))
        .map(|r| r.id)
        .max()
}

/// The end of `root`'s chain: follow `retry_of` links forward to the task
/// nothing yet retries.
pub fn tip_of(root: i64, rows: &[LineageRow]) -> i64 {
    let mut cur = root;
    while let Some(next) = newest_retry_of(cur, rows) {
        cur = next;
    }
    cur
}

/// What became of a lineage, from its tip's state, reason and landing
/// fields: queued, running or capped are still going (`InProgress`);
/// succeeded is `Landed` once it either landed (`landed_sha` set) or was
/// never meant to (`--no-land`, `land` false), else still `InProgress`
/// pending a human's `forge land` — the same "resolved" test
/// `queue::map_dep` uses; withdrawn and unverified keep their own name;
/// blocked is a `Question` when the reason is one addressed to a person,
/// else `Dangling` beside every failed tip.
pub fn classify(state: TaskState, reason: &str, land: bool, landed_sha: &str) -> Outcome {
    match state {
        TaskState::Queued | TaskState::Running | TaskState::Capped => Outcome::InProgress,
        TaskState::Succeeded => {
            if !land || !landed_sha.is_empty() {
                Outcome::Landed
            } else {
                Outcome::InProgress
            }
        }
        TaskState::Withdrawn => Outcome::Withdrawn,
        TaskState::Unverified => Outcome::Unverified,
        TaskState::Blocked => {
            if is_question(reason) {
                Outcome::Question
            } else {
                Outcome::Dangling
            }
        }
        TaskState::Failed => Outcome::Dangling,
    }
}

/// One lineage's audit row: its root and tip, what became of it, how many
/// tasks it took, and their summed attempt cost.
#[derive(Debug, Clone, Serialize)]
pub struct Lineage {
    pub root: i64,
    pub tip: i64,
    pub outcome: Outcome,
    pub task_count: usize,
    pub cost_usd: f64,
    /// The tip's own reason; the text behind a `Question` or `Dangling` outcome.
    pub reason: String,
}

/// One lineage from its rows (`Store::lineage(root)`'s output, root first):
/// its tip, outcome, task count and summed attempt cost. `None` for an
/// empty slice.
pub fn lineage_of(rows: &[LineageRow]) -> Option<Lineage> {
    let root = rows.iter().map(|r| r.id).min()?;
    let tip_id = tip_of(root, rows);
    let tip = rows.iter().find(|r| r.id == tip_id)?;
    let state = TaskState::try_from(tip.state.as_str()).ok()?;
    Some(Lineage {
        root,
        tip: tip_id,
        outcome: classify(state, &tip.reason, tip.land, &tip.landed_sha),
        task_count: rows.len(),
        cost_usd: rows.iter().map(|r| r.cost).sum(),
        reason: tip.reason.clone(),
    })
}

/// One outcome bucket's totals: every lineage classified with it, the
/// tasks across all of them, and what they spent.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Totals {
    pub lineages: usize,
    pub tasks: usize,
    pub cost_usd: f64,
}

/// A dangling tip: the task an operator has to look at by hand, and why.
#[derive(Debug, Clone, Serialize)]
pub struct DanglingTip {
    pub id: i64,
    pub reason: String,
}

/// `forge audit`'s report: every discovered lineage's outcome totaled
/// (every one of `Outcome::ALL`, even at zero), and the dangling tips
/// named with their reasons.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub outcomes: BTreeMap<Outcome, Totals>,
    pub dangling: Vec<DanglingTip>,
}

/// Totals every lineage into its outcome bucket and lists the dangling tips.
pub fn report(lineages: &[Lineage]) -> Report {
    let mut outcomes: BTreeMap<Outcome, Totals> = Outcome::ALL
        .into_iter()
        .map(|o| (o, Totals::default()))
        .collect();
    let mut dangling = Vec::new();
    for l in lineages {
        let t = outcomes.entry(l.outcome).or_default();
        t.lineages += 1;
        t.tasks += l.task_count;
        t.cost_usd += l.cost_usd;
        if l.outcome == Outcome::Dangling {
            dangling.push(DanglingTip {
                id: l.tip,
                reason: l.reason.clone(),
            });
        }
    }
    Report { outcomes, dangling }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, parent: Option<i64>, state: &str, reason: &str) -> LineageRow {
        LineageRow {
            id,
            parent,
            state: state.into(),
            reason: reason.into(),
            workflow: "direct".into(),
            cost: 1.0,
            land: true,
            landed_sha: if state == "succeeded" {
                "sha".into()
            } else {
                String::new()
            },
        }
    }

    #[test]
    fn tip_of_follows_a_straight_retry_chain() {
        let rows = [
            row(1, None, "failed", "x"),
            row(2, Some(1), "failed", "x"),
            row(3, Some(2), "succeeded", ""),
        ];
        assert_eq!(tip_of(1, &rows), 3);
    }

    #[test]
    fn tip_of_a_single_task_is_itself() {
        let rows = [row(1, None, "queued", "")];
        assert_eq!(tip_of(1, &rows), 1);
    }

    #[test]
    fn tip_of_a_branch_picks_the_newest_retry() {
        // A review demotion's follow-up (822) beside a hand retry (823),
        // both retrying 811: the newest is the one still live.
        let rows = [
            row(811, None, "blocked", "review demoted: off by one"),
            row(822, Some(811), "failed", "L1 failed: test"),
            row(823, Some(811), "succeeded", ""),
        ];
        assert_eq!(tip_of(811, &rows), 823);
    }

    #[test]
    fn classify_maps_every_state() {
        assert_eq!(
            classify(TaskState::Queued, "", true, ""),
            Outcome::InProgress
        );
        assert_eq!(
            classify(TaskState::Running, "", true, ""),
            Outcome::InProgress
        );
        assert_eq!(
            classify(TaskState::Capped, "task budget reached", true, ""),
            Outcome::InProgress
        );
        assert_eq!(
            classify(TaskState::Succeeded, "", true, "abc123"),
            Outcome::Landed
        );
        assert_eq!(
            classify(TaskState::Withdrawn, "stale", true, ""),
            Outcome::Withdrawn
        );
        assert_eq!(
            classify(TaskState::Unverified, "no L1 or L2", true, ""),
            Outcome::Unverified
        );
        assert_eq!(
            classify(TaskState::Failed, "L1 failed: test", true, ""),
            Outcome::Dangling
        );
    }

    #[test]
    fn a_succeeded_task_still_pending_forge_land_is_in_progress() {
        assert_eq!(
            classify(TaskState::Succeeded, "", true, ""),
            Outcome::InProgress
        );
    }

    #[test]
    fn a_no_land_succeeded_task_is_landed_with_no_sha() {
        assert_eq!(
            classify(TaskState::Succeeded, "", false, ""),
            Outcome::Landed
        );
    }

    #[test]
    fn a_question_addressed_to_a_person_is_not_dangling() {
        assert_eq!(
            classify(TaskState::Blocked, "needs input: which db?", true, ""),
            Outcome::Question
        );
    }

    #[test]
    fn a_blocked_task_with_no_open_question_is_dangling() {
        for reason in [
            "needs workflow: no e2e step",
            "needs suite: stars-tier.test.ts",
            "review demoted: off by one",
            "waits on task 14 (failed: L1 failed: test)",
        ] {
            assert_eq!(
                classify(TaskState::Blocked, reason, true, ""),
                Outcome::Dangling,
                "{reason}"
            );
        }
    }

    #[test]
    fn lineage_of_sums_task_count_and_cost_at_the_tip() {
        let rows = [
            row(1, None, "failed", "x"),
            row(2, Some(1), "failed", "L1 failed: test"),
        ];
        let l = lineage_of(&rows).unwrap();
        assert_eq!(l.root, 1);
        assert_eq!(l.tip, 2);
        assert_eq!(l.outcome, Outcome::Dangling);
        assert_eq!(l.task_count, 2);
        assert_eq!(l.cost_usd, 2.0);
        assert_eq!(l.reason, "L1 failed: test");
    }

    #[test]
    fn lineage_of_an_empty_slice_is_none() {
        assert!(lineage_of(&[]).is_none());
    }

    #[test]
    fn report_totals_every_outcome_and_lists_dangling_tips() {
        let landed = lineage_of(&[row(1, None, "succeeded", "")]).unwrap();
        let dangling =
            lineage_of(&[row(2, None, "failed", "operation stamp failed: boom")]).unwrap();
        let r = report(&[landed, dangling]);
        assert_eq!(r.outcomes.len(), 6, "every outcome present, even at zero");
        assert_eq!(r.outcomes[&Outcome::Landed].tasks, 1);
        assert_eq!(r.outcomes[&Outcome::Landed].cost_usd, 1.0);
        assert_eq!(r.outcomes[&Outcome::InProgress].tasks, 0);
        assert_eq!(r.outcomes[&Outcome::Dangling].tasks, 1);
        assert_eq!(r.dangling.len(), 1);
        assert_eq!(r.dangling[0].id, 2);
        assert_eq!(r.dangling[0].reason, "operation stamp failed: boom");
    }
}
