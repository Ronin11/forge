//! Invocation and phase inputs shared by the agent runners.

use super::*;

/// A Codex phase with its command, environment, and streaming output sinks.
pub(super) struct RunCodexPhase<'a> {
    pub(super) l: &'a Launch<'a>,
    pub(super) argv: &'a [String],
    pub(super) extra_env: &'a [(String, String)],
    pub(super) start: &'a Instant,
    pub(super) log: &'a mut File,
    pub(super) out: &'a mut Outcome,
    pub(super) watch: &'a mut Watch,
}

/// One agent invocation, including its sandbox, limits, identity, and output sinks.
pub(super) struct AgentRun<'a> {
    pub(super) sandbox: Option<&'a Execution>,
    pub(super) worktree: &'a Path,
    pub(super) argv: &'a [String],
    pub(super) identity: &'a [(String, String)],
    pub(super) prompt: &'a str,
    pub(super) bin: &'a str,
    pub(super) timeout: Duration,
    pub(super) writes: bool,
    pub(super) early_ending: crate::config::EarlyEnding,
    pub(super) task_id: i64,
    pub(super) report: &'a Reporter,
    pub(super) log: &'a mut File,
}

/// A JSON-streaming agent phase and the parser that folds frames into its outcome.
pub(super) struct RunJsonPhase<'a> {
    pub(super) l: &'a Launch<'a>,
    pub(super) argv: &'a [String],
    pub(super) extra_env: &'a [(String, String)],
    pub(super) start: &'a Instant,
    pub(super) log: &'a mut File,
    pub(super) out: &'a mut Outcome,
    pub(super) watch: &'a mut Watch,
    pub(super) apply:
        &'a mut (dyn FnMut(&Value, &mut Outcome, &mut Watch) -> Option<String> + Send + 'a),
}

/// A Copilot phase with its output sinks and cumulative token accounting.
pub(super) struct RunCopilotPhase<'a> {
    pub(super) l: &'a Launch<'a>,
    pub(super) argv: &'a [String],
    pub(super) extra_env: &'a [(String, String)],
    pub(super) start: &'a Instant,
    pub(super) log: &'a mut File,
    pub(super) out: &'a mut Outcome,
    pub(super) watch: &'a mut Watch,
    pub(super) tally: &'a mut CopilotTally,
}
