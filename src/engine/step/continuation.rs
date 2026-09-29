//! Choose the session and feedback for the next attempt after a failed result.
use super::*;

pub(super) fn after_failure(
    f: &Forge,
    t: &Task,
    ts: &Task,
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
    outcome: &crate::agent::Outcome,
) -> (Option<Resume>, Option<String>, bool) {
    let id = t.id;
    let resume;
    let feedback;
    // Out of turns before producing a result: continue the
    // same session rather than start over blind. Even with
    // nothing on the tree, the session holds what the agent
    // located; a fresh attempt would spend its turns finding
    // it again (33 of the first 214 attempts did exactly
    // that). A capped attempt that did return a result gets
    // the ordinary feedback for what its result failed.
    let capped = outcome.max_turns_hit || outcome.num_turns >= ts.max_turns;
    let stopped = outcome.ended_early.is_some();
    let unfinished = verdict.envelope.is_none();
    let progress = verdict.commits > 0 || verdict.dirty;
    let capped_committed = capped && unfinished && verdict.commits > 0 && !verdict.dirty;
    let over = fresh_arm(t)
        && std::fs::read_to_string(&a.log_path)
            .ok()
            .and_then(|l| crate::handoff::last_context_tokens(&l))
            .is_some_and(|n| n > crate::handoff::CONTEXT_THRESHOLD_TOKENS);
    if (capped || stopped || over)
        && unfinished
        && let Some(sid) = &outcome.session_id
    {
        f.report.emit(
            id,
            Event::Note {
                text: &format!(
                    "resume   continuing session {} {}",
                    &sid[..sid.len().min(8)],
                    if stopped {
                        "after stopping it early"
                    } else if capped {
                        "past the turn cap"
                    } else {
                        "past the context threshold"
                    }
                ),
            },
        );
        resume = Some(Resume {
            session: sid.clone(),
            start_sha: a.start_sha.clone(),
            fresh_from: fresh_arm(t).then(|| PathBuf::from(&a.log_path)),
        });
        feedback = Some(if let Some(why) = &outcome.ended_early {
            early_feedback(why, &outcome.early_signals)
        } else if progress {
            "You ran out of turns before finishing. Continue exactly where you left off: finish the work, leave the tree clean, commit, and return the structured result.".to_string()
        } else {
            "You ran out of turns before changing anything. You have already read what you need: stop exploring, make the change now, commit as soon as it compiles, and return the structured result.".to_string()
        });
    } else if t.resume_on_failure
        && a.state == AttemptState::ChecksFailed
        && let Some(sid) = &outcome.session_id
    {
        // The operator asked to keep going in the same
        // session after a failed attempt, not just a
        // capped one: same feedback, same CLI session.
        f.report.emit(
            id,
            Event::Note {
                text: &format!(
                    "resume   continuing session {} after failed checks",
                    &sid[..sid.len().min(8)]
                ),
            },
        );
        resume = Some(Resume {
            session: sid.clone(),
            start_sha: a.start_sha.clone(),
            fresh_from: over.then(|| PathBuf::from(&a.log_path)),
        });
        feedback = Some(verify::feedback(&verdict, &outcome, ts.max_turns));
    } else {
        resume = None;
        feedback = Some(verify::feedback(&verdict, &outcome, ts.max_turns));
    }
    (resume, feedback, capped_committed)
}
