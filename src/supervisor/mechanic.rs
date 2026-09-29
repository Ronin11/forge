//! Automatic follow-ups for mechanical failures. A task that ends
//! `TaskState::Failed` is classified into one of a small set of mechanical
//! kinds — a load flake, a landing conflict, a ratchet the branch tripped,
//! a turn cap reached with commits, or an L0 clean-tree failure — and the
//! kernel acts once per kind per lineage: a retry (twice for a landing
//! conflict), or, for a ratchet, a refile that supersedes the failed task
//! with guidance appended. Anything else, or a kind already spent on this
//! lineage, is left for the operator: a decision names the failure and
//! what was tried, and the task stays `Failed` (already actionable with
//! `forge retry`/`forge withdraw`, so nothing here reopens it).
//!
//! Every action, including the operator hand-off, is one decision row
//! (`Store::insert_decision_by` + `set_decision_kind`), so `forge stats`
//! can count them per kind (`Store::mechanic_kind_counts`) the same way
//! every other kernel ruling is recorded (docs/WORKFLOWS.md, "Mechanic").

mod flake;
mod ratchet;
mod reasons;

use crate::checks::CheckResult;
use crate::ctx::Forge;
use crate::queue::{self, RetryOverrides};
use crate::report::Event;
use crate::store::{InsertDecisionBy, Task, TaskState};
use crate::{config, git};
use anyhow::Result;
use std::path::Path;

/// A mechanical failure kind the kernel recognizes and acts on by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    LoadFlake,
    LandingConflict,
    Ratchet,
    TurnCap,
    CleanTree,
}

impl Kind {
    /// The `decisions.kind` value this action is recorded under; also
    /// `forge stats`' per-kind grouping key (`mechanic-` prefixed, so it
    /// never collides with an unrelated kernel ruling's own kind, e.g.
    /// `demotion-as-task`).
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::LoadFlake => "mechanic-load-flake",
            Kind::LandingConflict => "mechanic-landing-conflict",
            Kind::Ratchet => "mechanic-ratchet",
            Kind::TurnCap => "mechanic-turn-cap",
            Kind::CleanTree => "mechanic-clean-tree",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::LoadFlake => "load flake",
            Kind::LandingConflict => "landing conflict",
            Kind::Ratchet => "ratchet",
            Kind::TurnCap => "turn cap with commits",
            Kind::CleanTree => "L0 clean-tree",
        }
    }

    /// How many times this kind may act on one lineage: twice for a
    /// landing conflict (the base may move again while it retries through
    /// the integrator), once for everything else.
    fn budget(self) -> usize {
        match self {
            Kind::LandingConflict => 2,
            _ => 1,
        }
    }
}

/// The decision row's `kind` for the operator hand-off: not a mechanical
/// action, so it carries no `Kind` of its own.
const BLOCK_KIND: &str = "mechanic-block";

/// What was found and the evidence to cite for it.
struct Classified {
    kind: Kind,
    evidence: String,
}

fn last_verdict(attempts: &[crate::store::Attempt]) -> Vec<CheckResult> {
    attempts
        .iter()
        .rev()
        .find(|a| a.state == crate::store::AttemptState::ChecksFailed)
        .map(|a| serde_json::from_str(&a.verdict_json).unwrap_or_default())
        .unwrap_or_default()
}

/// The failed checks' failing tests, if the last attempt to fail checks had
/// exactly one such check: ambiguous otherwise, so no load-flake guess is
/// made from it.
fn sole_failing_check(verdict: &[CheckResult]) -> Option<&CheckResult> {
    let mut failed = verdict
        .iter()
        .filter(|c| !c.ok && !c.failing_tests.is_empty());
    let one = failed.next()?;
    failed.next().is_none().then_some(one)
}

/// Classify a failed task from its reason and its last verdict; the async
/// arm (load flake) actually re-runs the check against the base, so this is
/// the only part of classification that touches the filesystem or a clock.
async fn classify(
    f: &Forge,
    t: &Task,
    verdict: &[CheckResult],
    changed: &[String],
    cfg: &config::Config,
) -> Result<Option<Classified>> {
    if reasons::is_landing_conflict(&t.reason) {
        return Ok(Some(Classified {
            kind: Kind::LandingConflict,
            evidence: t.reason.clone(),
        }));
    }
    if reasons::is_turn_cap_with_commits(&t.reason) {
        return Ok(Some(Classified {
            kind: Kind::TurnCap,
            evidence: t.reason.clone(),
        }));
    }
    if reasons::is_clean_tree_only(&t.reason) {
        return Ok(Some(Classified {
            kind: Kind::CleanTree,
            evidence: t.reason.clone(),
        }));
    }
    if let Some(hit) = ratchet::detect(verdict) {
        return Ok(Some(Classified {
            kind: Kind::Ratchet,
            evidence: format!("{}: {}", hit.ratchet.label(), hit.guidance),
        }));
    }
    if let Some(check) = sole_failing_check(verdict)
        && !flake::touches_failing_tests(changed, &check.failing_tests)
        && flake::passes_on_base(f, t, cfg, check).await?
    {
        return Ok(Some(Classified {
            kind: Kind::LoadFlake,
            evidence: format!(
                "{} {}: {} pass on the base, untouched by this branch",
                check.level,
                check.name,
                check.failing_tests.join(", ")
            ),
        }));
    }
    Ok(None)
}

/// Every decision already recorded on `id`'s lineage with kind `kind`.
fn spent_in_lineage(f: &Forge, id: i64, kind: Kind) -> Result<usize> {
    Ok(f.store
        .decisions_in_lineage(id)?
        .iter()
        .filter(|d| d.kind == kind.as_str())
        .count())
}

/// Record the decision and retry once, with `overrides` and, when given, the
/// task text a retry re-queues with (the original text plus guidance).
async fn retry(
    f: &Forge,
    t: &Task,
    c: &Classified,
    overrides: RetryOverrides,
    text: Option<String>,
) -> Result<()> {
    let decision = f.store.insert_decision_by(InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: &format!("task {} failed: {}", t.id, t.reason),
        answer: &format!(
            "classified as a {}: {}; retrying",
            c.kind.label(),
            c.evidence
        ),
        answered_by: "mechanic",
        citations: &c.evidence,
        answered_for: None,
    })?;
    f.store.set_decision_kind(decision, c.kind.as_str())?;
    let after = t
        .after
        .iter()
        .map(|&d| queue::map_dep(f, d, &Default::default()))
        .collect::<Result<Vec<_>>>()?;
    let req = queue::retry_request(t, &overrides, true, after, text);
    let n = queue::enqueue(f, &req, Some(t.id)).await?;
    f.store.set_decision_retry(decision, n.id)?;
    f.report.emit(
        t.id,
        Event::Note {
            text: &format!("mechanic: {} — retried as task {}", c.kind.label(), n.id),
        },
    );
    Ok(())
}

/// Record the decision and file a plain new task carrying the original
/// text plus the ratchet's guidance, linked to the failed task only through
/// this decision's citations (task 1057 has not landed a `supersedes`
/// column yet; see the `TODO` below).
async fn refile(f: &Forge, t: &Task, c: &Classified) -> Result<()> {
    let decision = f.store.insert_decision_by(InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: &format!("task {} failed: {}", t.id, t.reason),
        answer: &format!(
            "classified as a {}: {}; refiling",
            c.kind.label(),
            c.evidence
        ),
        answered_by: "mechanic",
        citations: &format!("supersedes task {}: {}", t.id, c.evidence),
        answered_for: None,
    })?;
    f.store.set_decision_kind(decision, c.kind.as_str())?;
    let after = t
        .after
        .iter()
        .map(|&d| queue::map_dep(f, d, &Default::default()))
        .collect::<Result<Vec<_>>>()?;
    let text = format!("{}\n\n{}", t.task, c.evidence);
    let req = queue::retry_request(t, &RetryOverrides::none(), true, after, Some(text));
    // TODO(#1057 --supersedes): once the supersedes column and `forge add
    // --supersedes` land, file this with `--supersedes t.id` and drop the
    // citation-only link above; `retry_of` must stay `None` either way,
    // since a refile starts a fresh lineage rather than retrying this one.
    let n = queue::enqueue(f, &req, None).await?;
    f.store.set_decision_retry(decision, n.id)?;
    f.report.emit(
        t.id,
        Event::Note {
            text: &format!(
                "mechanic: ratchet — refiled as task {} (supersedes {})",
                n.id, t.id
            ),
        },
    );
    Ok(())
}

/// Record the operator hand-off: no action taken, the task stays `Failed`.
fn block(f: &Forge, t: &Task, tried: &str) -> Result<()> {
    let decision = f.store.insert_decision_by(InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: &format!("task {} failed: {}", t.id, t.reason),
        answer: &format!(
            "not a recognized mechanical follow-up (or that kind was already spent on this lineage): {tried}. Needs the operator: forge retry {} or forge withdraw {}",
            t.id, t.id
        ),
        answered_by: "mechanic",
        citations: tried,
        answered_for: None,
    })?;
    f.store.set_decision_kind(decision, BLOCK_KIND)?;
    f.report.emit(
        t.id,
        Event::Note {
            text: &format!("mechanic: raised to the operator ({tried})"),
        },
    );
    Ok(())
}

/// The kernel's rule on a task that just ended `TaskState::Failed`: read
/// `docs/WORKFLOWS.md`, "Mechanic", for the shape of each kind. Idempotent
/// in effect if ever called twice on the same failed task, since the
/// second call sees its own first decision already spending that kind's
/// budget and blocks instead of acting again.
pub async fn act(f: &Forge, id: i64) -> Result<()> {
    let Some(t) = f.store.task(id)? else {
        return Ok(());
    };
    if t.state != TaskState::Failed || !reasons::in_scope(&t.reason) {
        return Ok(());
    }
    let attempts = f.store.attempts(id)?;
    let verdict = last_verdict(&attempts);
    let wt = Path::new(&t.worktree);
    let changed = git::changed_paths(wt, &t.base_sha)
        .await
        .unwrap_or_default();
    let cfg = match config::load_at(Path::new(&t.repo), wt, &t.base_sha).await {
        Ok(cfg) => cfg,
        Err(e) => return block(f, &t, &format!("could not classify: {e:#}")),
    };
    let Some(c) = classify(f, &t, &verdict, &changed, &cfg).await? else {
        return block(f, &t, &format!("no mechanical kind matched: {}", t.reason));
    };
    let spent = spent_in_lineage(f, id, c.kind)?;
    if spent >= c.kind.budget() {
        return block(
            f,
            &t,
            &format!(
                "{} already tried {} time(s) on this lineage: {}",
                c.kind.label(),
                spent,
                c.evidence
            ),
        );
    }
    match c.kind {
        Kind::LoadFlake | Kind::LandingConflict => {
            retry(f, &t, &c, RetryOverrides::none(), None).await
        }
        Kind::TurnCap => {
            let doubled = (t.max_turns.max(1) as u32) * 2;
            retry(
                f,
                &t,
                &c,
                RetryOverrides {
                    max_turns: Some(doubled),
                    ..RetryOverrides::none()
                },
                None,
            )
            .await
        }
        Kind::CleanTree => {
            let text = format!(
                "{}\n\ncommit or gitignore everything; git status --porcelain must print nothing",
                t.task
            );
            retry(f, &t, &c, RetryOverrides::none(), Some(text)).await
        }
        Kind::Ratchet => refile(f, &t, &c).await,
    }
}

#[cfg(test)]
mod tests;
