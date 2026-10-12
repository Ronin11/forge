//! Infra failure is not a strike. An attempt that died because the host
//! was starved or the provider's API failed before the agent could work
//! is refunded like a refused run, never counted against the task, and the
//! same session goes again; `[infra] limit` such deaths in a row fail the
//! task uncounted so a dead host never loops. The host is sampled the
//! moment the agent process ends (`host::Sample::now`, before any check
//! runs), and the attempt's outputs keep the sample
//! (docs/WORKFLOWS.md, "Infra failures").

use super::attempts::{AttemptFlow, AttemptLoop};
use super::*;
use crate::agent::Outcome;

/// What a provider or network failure looks like in a result frame's text,
/// the CLI's terminal diagnosis or its stderr; matched lowercase. A 4xx
/// is never here: a bad request or a refused login is someone's fault.
const PROVIDER_ERRORS: &[&str] = &[
    "api error: 5",
    "overloaded",
    "internal server error",
    "bad gateway",
    "service unavailable",
    "gateway timeout",
    "connection error",
    "network error",
    "fetch failed",
    "socket hang up",
    "econnreset",
    "econnrefused",
    "etimedout",
    "enotfound",
    "eai_again",
];

/// The first line of `text` naming a provider or network failure.
fn provider_error_line(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let lower = l.to_ascii_lowercase();
        PROVIDER_ERRORS
            .iter()
            .any(|p| lower.contains(p))
            .then(|| l.trim().chars().take(160).collect())
    })
}

/// Why this death was the host's or the provider's, when it was: a
/// timeout on a starved host, or no result inside `router_secs` with a
/// provider error in the output. A refusal is judged elsewhere, first.
pub(super) fn classify(outcome: &Outcome, cfg: &crate::config::Infra) -> Option<String> {
    if outcome.rate_limited || outcome.login_refused || outcome.budget_hit {
        return None;
    }
    if outcome.timed_out {
        return outcome.host.filter(|h| h.starved(cfg)).map(|h| {
            format!(
                "the host was starved when the attempt timed out ({})",
                h.describe()
            )
        });
    }
    let answered = outcome.got_result && !outcome.is_error;
    if answered || outcome.wall_ms >= cfg.router_secs as u128 * 1000 {
        return None;
    }
    [
        outcome.result_text.as_str(),
        outcome.terminal_reason.as_deref().unwrap_or(""),
        outcome.stderr_text.as_str(),
    ]
    .into_iter()
    .find_map(provider_error_line)
    .map(|line| format!("the provider failed before the agent could work: {line}"))
}

/// Absorb an infra death: refund it, keep the session to go again, and
/// fail the task uncounted past `[infra] limit` in a row. `None` when
/// the death was not infra (and the streak resets).
pub(super) fn absorb(
    f: &Forge,
    state: &mut AttemptLoop,
    run: &mut Run,
    seq: i64,
    id: i64,
    a: &crate::store::Attempt,
    outcome: &Outcome,
) -> Result<Option<AttemptFlow>, Fault> {
    let Some(why) = classify(outcome, &f.infra) else {
        state.consecutive_infra = 0;
        return Ok(None);
    };
    state.consecutive_infra += 1;
    if state.consecutive_infra > f.infra.limit {
        return Ok(Some(AttemptFlow::Exit(StepFlow::End(End::Failed {
            reason: format!(
                "{} infra failures in a row, none counted; the last: {why}",
                f.infra.limit
            ),
            counted: false,
            pushes: false,
        }))));
    }
    f.report.emit(
        id,
        Event::Note {
            text: &format!("infra    {why}; it does not count as an attempt"),
        },
    );
    run.refund(f, seq, a.id)?;
    if let Some(sid) = &outcome.session_id {
        state.resume = Some(Resume {
            session: sid.clone(),
            start_sha: a.start_sha.clone(),
            fresh_from: None,
            cold: false,
        });
    }
    Ok(Some(AttemptFlow::Continue))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{Fixture, verdict};
    use super::*;

    fn cfg() -> crate::config::Infra {
        crate::config::Infra {
            load_per_core: 5.0,
            swap_gb: 4.0,
            router_secs: 600,
            limit: 3,
        }
    }

    fn starved() -> Option<crate::host::Sample> {
        Some(crate::host::Sample {
            load_per_core: Some(9.0),
            swap_used_gb: Some(0.0),
        })
    }

    #[test]
    fn a_timeout_is_infra_only_on_a_starved_host() {
        let out = Outcome {
            timed_out: true,
            host: starved(),
            ..Default::default()
        };
        let why = classify(&out, &cfg()).unwrap();
        assert!(why.starts_with("the host was starved"), "{why}");
        assert!(why.contains("load 9.0/core"), "{why}");
        let calm = Outcome {
            timed_out: true,
            host: Some(crate::host::Sample {
                load_per_core: Some(1.0),
                swap_used_gb: Some(0.0),
            }),
            ..Default::default()
        };
        assert_eq!(classify(&calm, &cfg()), None);
        let unsampled = Outcome {
            timed_out: true,
            ..Default::default()
        };
        assert_eq!(classify(&unsampled, &cfg()), None);
    }

    #[test]
    fn a_quick_death_on_a_provider_error_is_infra_and_a_slow_or_answered_one_is_not() {
        let out = Outcome {
            exit_code: Some(1),
            wall_ms: 12_000,
            stderr_text: "warn: something\nAPI Error: 529 {\"type\":\"overloaded_error\"}\n".into(),
            ..Default::default()
        };
        let why = classify(&out, &cfg()).unwrap();
        assert_eq!(
            why,
            "the provider failed before the agent could work: API Error: 529 {\"type\":\"overloaded_error\"}"
        );
        let in_result = Outcome {
            got_result: true,
            is_error: true,
            wall_ms: 3_000,
            result_text: "fetch failed".into(),
            ..Default::default()
        };
        assert!(classify(&in_result, &cfg()).is_some());
        let slow = Outcome {
            wall_ms: 601_000,
            ..out
        };
        assert_eq!(classify(&slow, &cfg()), None);
        let answered = Outcome {
            got_result: true,
            wall_ms: 1_000,
            stderr_text: "ECONNRESET once, then fine".into(),
            ..Default::default()
        };
        assert_eq!(classify(&answered, &cfg()), None);
        let bad_request = Outcome {
            got_result: true,
            is_error: true,
            wall_ms: 1_000,
            result_text: "API Error: 400 invalid request".into(),
            ..Default::default()
        };
        assert_eq!(classify(&bad_request, &cfg()), None);
        let refused = Outcome {
            rate_limited: true,
            wall_ms: 1_000,
            result_text: "API Error: 529 overloaded".into(),
            ..Default::default()
        };
        assert_eq!(
            classify(&refused, &cfg()),
            None,
            "a refusal is judged first"
        );
    }

    #[tokio::test]
    async fn an_infra_death_is_refunded_and_resumed_and_the_streak_fails_uncounted() {
        let mut x = Fixture::new();
        let a = x.attempt(AttemptState::AgentFailed);
        let ts = x.t.clone();
        let v = verdict();
        let outcome = Outcome {
            timed_out: true,
            host: starved(),
            session_id: Some("session".into()),
            ..Default::default()
        };
        let mut state = AttemptLoop {
            feedback: Some("fix this".into()),
            ..Default::default()
        };
        let limit = x.f.infra.limit;
        for _ in 0..limit {
            x.run.used.insert(2, 1);
            let flow =
                bookkeeping::retry_after_attempt(&mut x.args(), &mut state, &ts, &a, &v, &outcome)
                    .await
                    .ok()
                    .unwrap();
            assert!(matches!(flow, Some(AttemptFlow::Continue)));
            assert_eq!(x.run.used_at(2), 0, "refunded");
            assert_eq!(state.feedback.as_deref(), Some("fix this"));
            let r = state.resume.as_ref().unwrap();
            assert_eq!(r.session, "session");
            assert!(r.fresh_from.is_none());
        }
        assert!(x.f.store.refunded_attempts(x.t.id).unwrap().contains(&a.id));
        x.run.used.insert(2, 1);
        let flow =
            bookkeeping::retry_after_attempt(&mut x.args(), &mut state, &ts, &a, &v, &outcome)
                .await
                .ok()
                .unwrap();
        let Some(AttemptFlow::Exit(StepFlow::End(End::Failed {
            reason,
            counted,
            pushes,
        }))) = flow
        else {
            panic!("the streak past the limit ends the step");
        };
        assert!(reason.starts_with("3 infra failures in a row"), "{reason}");
        assert!(!counted && !pushes);
        // A death that was the agent's own resets the streak.
        let own = Outcome::default();
        state.consecutive_infra = 2;
        bookkeeping::retry_after_attempt(&mut x.args(), &mut state, &ts, &a, &v, &own)
            .await
            .ok()
            .unwrap();
        assert_eq!(state.consecutive_infra, 0);
    }
}
