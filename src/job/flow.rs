//! Where a run goes after a step (docs/EXECUTION.md, "Outcomes, then
//! edges"): the `on` entry for the outcome the directive returned or for
//! failure, else the list order; a loop is bounded by the target step's
//! attempt cap and by nothing else.

use super::{FailureAction, ask, failure_reason, retry_job};
use crate::checks;
use crate::ctx::Forge;
use crate::store::Job;
use crate::workflows::{RunStep, edges};
use std::path::Path;

pub(super) enum Route {
    Go(usize),
    End,
    /// A failure with no edge: the run fails.
    Failed,
    /// A loop entered a step that has spent its attempts.
    Capped(String),
}

/// The step the cursor takes next after `at`, given whether it failed and
/// the outcome it returned; `runs` counts how often each step has started.
pub(super) fn next_step(
    steps: &[RunStep],
    at: usize,
    failed: bool,
    outcome: &str,
    runs: &[u32],
) -> Route {
    let key = if failed { edges::FAILURE } else { outcome };
    let target = match steps[at].on.get(key) {
        Some(t) if t == edges::END => return Route::End,
        Some(t) => steps
            .iter()
            .position(|s| &s.node == t)
            .unwrap_or(steps.len()),
        None if failed => return Route::Failed,
        None => at + 1,
    };
    if target >= steps.len() {
        return Route::End;
    }
    loop_bound(steps, target, runs).map_or(Route::Go(target), Route::Capped)
}

/// The refusal message when entering `to` again would exceed its cap.
fn loop_bound(steps: &[RunStep], to: usize, runs: &[u32]) -> Option<String> {
    (runs[to] >= steps[to].max_attempts).then(|| {
        format!(
            "step {} ran {} time(s), its attempt cap; the loop stops here",
            steps[to].node, runs[to]
        )
    })
}

/// What `[limits] on_failure` decided, done once the job row is final.
pub(super) async fn apply_on_failure(
    f: &Forge,
    job_row: &Job,
    action: FailureAction,
    verdict: &[checks::CheckResult],
    (project, workflow, repo): (&str, &str, &Path),
    input_text: &str,
) {
    let job_id = job_row.id;
    match action {
        FailureAction::Stop => {}
        FailureAction::Retry => {
            if let Err(e) = retry_job(f, job_row, input_text).await {
                eprintln!("job {job_id} retry: {e:#}");
            }
        }
        FailureAction::Ask(to) => {
            let effects = f.store.job_effects(job_id).unwrap_or_default();
            let reason = failure_reason(job_id, workflow, verdict, &effects);
            let repo = repo.display().to_string();
            if let Err(e) = ask(f, project, &repo, to.as_deref(), reason) {
                eprintln!("job {job_id} ask: {e:#}");
            }
        }
    }
}
