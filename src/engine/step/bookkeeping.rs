//! Refund and rewind a coder when verification failed only in the tests author's namespace.
use super::*;

pub(super) async fn rewind_tests(
    args: &mut RunDirectiveStep<'_>,
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
) -> Result<Option<StepFlow>, Fault> {
    let f = args.f;
    let t = &mut *args.t;
    let cfg = args.cfg;
    let resolved = args.resolved;
    let run = &mut *args.run;
    let step = args.step;
    let seq = args.seq;
    let id = t.id;
    // A check that failed only inside the verification namespace
    // is the test author's failure, not the coder's: the coder
    // cannot see those files. Back to the tests step, within its
    // attempts; this attempt does not count against the coder.
    if a.state == AttemptState::ChecksFailed
        && step.action.contract != Contract::Tests
        && let Some((check, tail)) = verify::tests_fault(&verdict.checks, &cfg.namespace)
        && let Some(t_idx) = (0..run.idx)
            .rev()
            .find(|&i| resolved.steps[i].action.contract == Contract::Tests)
    {
        let t_seq = t_idx as i64 + 1;
        let t_used = run.used_at(t_seq);
        if t_used < t.max_attempts {
            run.refund(f, seq, a.id)?;
            f.report.emit(
                id,
                Event::Note {
                    text: &format!(
                        "verify   {check} failed inside {}; back to {} for another attempt",
                        cfg.namespace.join(" "),
                        resolved.steps[t_idx].action.name
                    ),
                },
            );
            run.rewind(t_idx, format!("The repository's `{check}` check failed on the implementer's tree, and every error is inside your tests:\n{tail}\nThe implementer cannot see or edit those files. Fix your tests so the repository's checks pass with them in place, commit, and describe the interface again."));
            // The coder starts over against the corrected tests.
            git::reset_hard(Path::new(&t.worktree), &t.base_sha)
                .await
                .task()?;
            return Ok(Some(StepFlow::Again));
        }
        return Ok(Some(StepFlow::End(End::Failed {
            reason: format!(
                "check {check} failed inside the verification namespace after {t_used} tests attempt(s): {}",
                tail.lines().next().unwrap_or("")
            ),
            counted: true,
            pushes: false,
        })));
    }
    Ok(None)
}
