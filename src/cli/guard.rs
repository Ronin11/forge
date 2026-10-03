//! `forge guard`: the landing guard's own callback, not a person's command
//! (see `crate::guard`, `deploy/pre-receive.guard`). The pre-receive hook
//! shells out to `forge guard record-override` when a push carries
//! `forge-override=<reason>`, so the emergency override is logged as a
//! decision from inside the hook's own process, with `FORGE_HOME` set to
//! the home the hook's git config names.

use super::*;

#[derive(Subcommand)]
pub(super) enum GuardCmd {
    /// Record the landing guard's emergency override as a decision, so
    /// `forge doctor` reports it for the next 24 hours (see
    /// `Store::insert_override_decision`).
    RecordOverride {
        /// The repository's path as Forge has it registered
        #[arg(long)]
        repo: String,
        /// The OS user who ran the push
        #[arg(long)]
        pusher: String,
        /// `forge-override`'s value: why the push bypassed the integrator
        #[arg(long)]
        reason: String,
        /// The base branch pushed to
        #[arg(long)]
        branch: String,
    },
}

pub(super) fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Guard { cmd } => match cmd {
            GuardCmd::RecordOverride {
                repo,
                pusher,
                reason,
                branch,
            } => {
                let f = Forge::open(false, false)?;
                f.store
                    .insert_override_decision(&repo, &pusher, &reason, &branch)?;
                Ok(())
            }
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}
