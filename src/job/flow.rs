//! Where a run goes after a step (docs/EXECUTION.md, "Outcomes, then
//! edges"): the `on` entry for the outcome the directive returned or for
//! failure, else the list order; a loop is bounded by the target step's
//! attempt cap and by nothing else.

use crate::workflows::{RunStep, edges};

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
