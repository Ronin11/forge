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
    // A context past the threshold continues fresh on the fresh arm. The
    // threshold is lower when the session's cache is forecast cold: the
    // resumed session would re-read all of it at the input price, where a
    // fresh one re-reads only the shared prefix (docs/CONTEXT.md).
    let cold = crate::handoff::cache_cold(a, f.cache.ttl_secs, crate::unix_now());
    let threshold = if cold {
        f.cache.cold_context_tokens
    } else {
        crate::handoff::CONTEXT_THRESHOLD_TOKENS
    };
    let over = fresh_arm(t)
        && std::fs::read_to_string(&a.log_path)
            .ok()
            .and_then(|l| crate::handoff::last_context_tokens(&l))
            .is_some_and(|n| n > threshold);
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
                    } else if cold {
                        "past the cold-cache context threshold"
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
            cold,
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
            cold,
        });
        feedback = Some(verify::feedback(verdict, outcome, ts.max_turns));
    } else {
        resume = None;
        feedback = Some(verify::feedback(verdict, outcome, ts.max_turns));
    }
    (resume, feedback, capped_committed)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{Fixture, verdict};
    use super::*;

    #[test]
    fn clean_committed_cap_keeps_session_and_marks_salvageable_work() {
        let x = Fixture::new();
        let a = x.attempt(AttemptState::AgentFailed);
        let mut v = verdict();
        v.commits = 1;
        let outcome = crate::agent::Outcome {
            max_turns_hit: true,
            session_id: Some("session".into()),
            ..Default::default()
        };
        let (resume, feedback, committed) = after_failure(&x.f, &x.t, &x.t, &a, &v, &outcome);
        assert!(committed);
        let resume = resume.unwrap();
        assert_eq!(resume.session, "session");
        assert_eq!(resume.start_sha, a.start_sha);
        assert!(
            feedback
                .unwrap()
                .contains("Continue exactly where you left off")
        );
        v.dirty = true;
        assert!(!after_failure(&x.f, &x.t, &x.t, &a, &v, &outcome).2);
    }

    /// A log whose last request carried `n` tokens of context.
    fn log_with_context(dir: &std::path::Path, n: i64) -> String {
        let p = dir.join("attempt.log");
        std::fs::write(
            &p,
            format!(
                "{{\"type\":\"assistant\",\"message\":{{\"usage\":{{\"input_tokens\":7,\"cache_read_input_tokens\":{}}}}}}}\n",
                n - 7
            ),
        )
        .unwrap();
        p.display().to_string()
    }

    #[test]
    fn a_cold_session_hands_off_past_the_lower_threshold_only_on_the_fresh_arm() {
        let mut x = Fixture::new();
        x.t.explore.insert("continuation".into(), "fresh".into());
        let mut a = x.attempt(AttemptState::ChecksFailed);
        a.log_path = log_with_context(x.dir.path(), 70_000);
        let mut v = verdict();
        v.envelope = Some(crate::envelope::Envelope::default());
        let outcome = crate::agent::Outcome {
            session_id: Some("session".into()),
            ..Default::default()
        };
        x.t.resume_on_failure = true;
        // The agent died an hour ago: the 5-minute cache is cold, and 70k
        // is past the cold threshold (60k) though not the warm one (120k).
        a.started_at = crate::unix_now() - 3600;
        a.agent_ms = 1_000;
        let (resume, _, _) = after_failure(&x.f, &x.t, &x.t, &a, &v, &outcome);
        let r = resume.expect("resumes on failure when asked");
        assert!(r.cold);
        assert!(r.fresh_from.is_some(), "cold and past 60k: fresh");
        // The same session a few seconds dead is warm: resumed as it was.
        a.started_at = crate::unix_now() - 10;
        let (resume, _, _) = after_failure(&x.f, &x.t, &x.t, &a, &v, &outcome);
        let r = resume.unwrap();
        assert!(!r.cold);
        assert!(r.fresh_from.is_none(), "warm and under 120k: resume");
        // Off the fresh arm the forecast changes nothing but the record.
        x.t.explore.clear();
        a.started_at = crate::unix_now() - 3600;
        let (resume, _, _) = after_failure(&x.f, &x.t, &x.t, &a, &v, &outcome);
        let r = resume.unwrap();
        assert!(r.cold && r.fresh_from.is_none());
    }

    #[test]
    fn failed_checks_resume_only_when_requested_and_keep_verification_feedback() {
        let mut x = Fixture::new();
        let a = x.attempt(AttemptState::ChecksFailed);
        let mut v = verdict();
        v.envelope = Some(crate::envelope::Envelope::default());
        let outcome = crate::agent::Outcome {
            session_id: Some("session".into()),
            ..Default::default()
        };
        for requested in [false, true] {
            x.t.resume_on_failure = requested;
            let (resume, feedback, committed) = after_failure(&x.f, &x.t, &x.t, &a, &v, &outcome);
            assert_eq!(resume.is_some(), requested);
            assert_eq!(
                feedback,
                Some(verify::feedback(&v, &outcome, x.t.max_turns))
            );
            assert!(!committed);
        }
    }
}
