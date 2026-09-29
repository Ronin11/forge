//! The attempts within one directive and their recorded verification rows.
use super::*;

pub(super) fn record_verification(
    f: &Forge,
    id: i64,
    seq: i64,
    timer: &Timer,
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
) -> Result<(), Fault> {
    // A deterministic fix ran before this verdict was
    // decided (see `verify::try_known_fix`): its own
    // row, so the trace shows what Forge did without an
    // agent turn before showing whether it worked.
    if let Some(fix) = &verdict.known_fix {
        op(
            f,
            id,
            timer,
            OpRow {
                seq,
                name: "known-fix",
                kernel: true,
                ok: fix.ok,
                exit: None,
                detail: &match &fix.commit {
                    Some(sha) => format!(
                        "{} fixed as {}: {}",
                        fix.checks.join(", "),
                        &sha[..8],
                        fix.diff_stat
                    ),
                    None => format!("{} left nothing to commit", fix.checks.join(", ")),
                },
                attempt_id: Some(a.id),
                output: "",
            },
        )?;
    }
    // The kernel's verify, as a row of its own.
    op(
        f,
        id,
        timer,
        OpRow {
            seq,
            name: "verify",
            kernel: true,
            ok: a.state == AttemptState::Succeeded,
            exit: None,
            detail: &a.reason,
            attempt_id: Some(a.id),
            output: "",
        },
    )?;
    Ok(())
}
