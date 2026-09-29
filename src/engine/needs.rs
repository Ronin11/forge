//! An environment need found in a failure's text: recognized, applied by
//! policy, or put to the supervisor, whose approval is applied the same
//! way.

use super::*;

/// The environment path after a directive's attempt failed: a failed check
/// or a question can name a need; an agent's own exit never does (its
/// tools' output is not on the record as a cause). Hosts that succeeded
/// attempts were also refused are dropped before reading.
pub(super) async fn environment_after(
    f: &Forge,
    t: &Task,
    cfg: &config::Config,
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
) -> Result<Environment, Fault> {
    if !matches!(
        a.state,
        AttemptState::ChecksFailed | AttemptState::NeedsInput
    ) {
        return Ok(Environment::Left);
    }
    let noise = f
        .store
        .hosts_refused_in_succeeded_attempts(crate::unix_now() - 7 * 86_400)
        .unwrap_or_default();
    apply_environment(f, t, cfg, &environment_text(a, verdict, &noise)).await
}

/// The text an environment need is read from: the failing checks' tails,
/// the attempt's reason and the question it asked, then each host the
/// egress proxy refused it, in the proxy's words, so a refusal a tool
/// swallowed is still a need.
fn environment_text(
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
    noise: &std::collections::HashSet<String>,
) -> String {
    let mut text = a.reason.clone();
    for c in verdict.checks.iter().filter(|c| !c.ok) {
        text.push('\n');
        text.push_str(&c.tail);
    }
    if let Some(q) = verdict
        .envelope
        .as_ref()
        .and_then(|e| e.needs_input.as_ref())
    {
        text.push('\n');
        text.push_str(&q.question);
    }
    let outputs: crate::audit::Outputs = serde_json::from_str(&a.outputs_json).unwrap_or_default();
    let worth = crate::environment::refusals_worth_reading(a.state, &outputs.refused, noise);
    text.push_str(&crate::environment::refusal_text(&worth));
    text
}

/// What became of an environment need found in a failure's text.
pub(super) enum Environment {
    /// Nothing recognized, covered or approved: the failure stands.
    Left,
    /// A grant was applied and recorded: the caller runs again.
    Applied,
    /// The supervisor denied it: the task blocks on this question.
    Ask(String),
}

/// Recognize an environment need in `text`. When the policy covers it,
/// apply it to the task's worktree and record the decision row by `forge`;
/// otherwise a host or cache need goes to the supervisor, whose approval
/// within the ceiling is applied and recorded the same way, by `supervisor`
/// (`env_supervisor`).
pub(super) async fn apply_environment(
    f: &Forge,
    t: &Task,
    cfg: &config::Config,
    text: &str,
) -> Result<Environment, Fault> {
    use crate::environment::Approval;
    let Some(need) = crate::environment::recognize(text) else {
        return Ok(Environment::Left);
    };
    let (grant, by) = if f.environment.covers(&need).is_some() {
        match f.grant_environment(Path::new(&t.worktree), &need, t.trust) {
            Some(g) => (g, Approval::Policy),
            None => return Ok(Environment::Left),
        }
    } else if crate::env_supervisor::applies(f, t, &need) {
        match crate::env_supervisor::rule(f, t, &cfg.environment_deny, &need)
            .await
            .env()?
        {
            crate::env_supervisor::Ruled::Approved(g, why) => {
                match f.apply_grant(Path::new(&t.worktree), g, t.trust) {
                    Some(g) => (g, Approval::Supervisor(why)),
                    None => return Ok(Environment::Left),
                }
            }
            crate::env_supervisor::Ruled::Denied(why) => {
                let q = crate::env_supervisor::question(&need, &why);
                crate::env_supervisor::block(f, t, &need, &q).env()?;
                return Ok(Environment::Ask(q));
            }
        }
    } else {
        return Ok(Environment::Left);
    };
    crate::environment::record(&f.store, t.id, &t.repo, &need, &grant, &by).env()?;
    f.report.emit(
        t.id,
        Event::Note {
            text: &format!(
                "environment {} {} granted ({}); running again, nothing counted",
                need.kind.as_str(),
                need.target,
                grant.describe()
            ),
        },
    );
    Ok(Environment::Applied)
}
