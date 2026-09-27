//! What a rejected base push means for a landing round: split out of
//! `integrate` to keep that function within its length ceiling.

use crate::ctx::Forge;
use crate::engine::{Fault, OpRow, Timer, op};
use crate::git;
use crate::report::Event;
use crate::store::Task;

/// What the caller does once a base push has been rejected.
pub(super) enum BasePush {
    /// The base moved again during this round; re-fetch it and merge once
    /// more.
    Retry,
    /// Nothing another round or the coder can do about it.
    Failed(String),
}

/// The fields `on_base_push_failure` needs: the round it happened in, the
/// remote it happened on, the op row it records, and the base it was
/// rejected against.
pub(super) struct BasePushFailure<'a> {
    pub(super) t: &'a Task,
    pub(super) url: &'a str,
    pub(super) main_sha: &'a str,
    pub(super) seq: i64,
    pub(super) timer: &'a Timer,
    pub(super) round: i32,
    pub(super) error: anyhow::Error,
}

/// `git::push_sha` of the base branch failed with `args.error`. Tell a base
/// that actually moved (worth another round) apart from every other
/// rejection, which is the environment's problem, not the task's: verifying
/// again cannot change a dirty checkout behind `updateInstead`, a hook, or
/// a permission.
pub(super) async fn on_base_push_failure(
    f: &Forge,
    args: BasePushFailure<'_>,
) -> Result<BasePush, Fault> {
    let BasePushFailure {
        t,
        url,
        main_sha,
        seq,
        timer,
        round,
        error,
    } = args;
    let d = format!("fast-forward of {} rejected: {error:#}", t.base_branch);
    op(
        f,
        t.id,
        timer,
        OpRow {
            seq,
            name: "land",
            kernel: true,
            ok: false,
            exit: None,
            detail: &d,
            attempt_id: None,
            output: "",
        },
    )?;
    let now = git::remote_branch_sha(url, &t.base_branch).await;
    if !now.is_some_and(|s| s != main_sha) {
        return Err(Fault::Env(anyhow::anyhow!(
            "{d}; {} did not move, so the remote refused it",
            t.base_branch
        )));
    }
    if round < 2 {
        f.report.emit(
            t.id,
            Event::Note {
                text: &format!(
                    "land     {} moved underneath; integrating again",
                    t.base_branch
                ),
            },
        );
        return Ok(BasePush::Retry);
    }
    Ok(BasePush::Failed(d))
}
