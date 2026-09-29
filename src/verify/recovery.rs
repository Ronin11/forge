//! A missing final report must not discard committed work before checks see it.

use super::l0_checks::derive_changes;
use super::*;
use anyhow::Result;
use crate::directive::{self, Failure};

/// Recover only against this attempt's start, never a previous attempt's
/// commits. Refusals and runs stopped by the kernel keep their own handling.
pub(super) async fn resolve(
    s: &Subject<'_>,
    agent: &Outcome,
    common: &mut Common,
) -> Result<(Option<String>, bool)> {
    let failure = directive::failure(agent);
    if common.envelope.is_some()
        || matches!(
            failure,
            Some(
                Failure::RateLimited
                    | Failure::LoginRefused
                    | Failure::EndedEarly(_)
                    | Failure::TimedOut
            )
        )
    {
        return Ok((failure.map(|f| f.reason()), false));
    }
    if crate::git::count_commits(s.worktree, s.start_sha).await? == 0 {
        let reason = match failure {
            Some(f) => f.reason(),
            None => "envelope missing or unparseable; no commits in this attempt".into(),
        };
        return Ok((Some(reason), false));
    }

    // This substitutes only for the report. Every other L0 rule and the
    // step's L1/L2 checks still decide the verdict on the existing tree.
    common.envelope = Some(Envelope {
        schema_version: 1,
        summary: "envelope missing; verification pending".into(),
        needs_input: None,
        changes: derive_changes(s.worktree, s.start_sha).await?,
        checks_run: Vec::new(),
        claims: Vec::new(),
        review_notes: Vec::new(),
    });
    if let Some(row) = common
        .rows
        .iter_mut()
        .find(|row| row.name == Rule::ResultStructured.name())
    {
        *row = l0(
            Rule::ResultStructured,
            true,
            "envelope missing; checking committed work".into(),
        );
    }
    common
        .rows
        .push(l0(Rule::ChangesFromGit, true, String::new()));
    common
        .rows
        .push(l0(Rule::ClaimsHaveEvidence, true, String::new()));
    Ok((None, true))
}
