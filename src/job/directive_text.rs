//! The text a directive job step is launched with: its instructions and
//! its inputs (docs/JOBS.md, "Steps").

use crate::workflows;

use super::bounded;

/// A directive job step's system content (docs/JOBS.md, "Steps"): the
/// untrusted-data sentence every Forge prompt carries, and the step's own
/// instructions — the action's description and its own `prompt`, if any.
/// Kept apart from the inputs (`directive_prompt`, below) so `Runner::Chat`,
/// which has its own system channel, does not have to guess where a
/// merged prompt's instructions end and its data begins.
pub(super) fn directive_instructions(action: &workflows::ActionDef) -> String {
    let mut p = String::from(workflows::UNTRUSTED_DATA);
    p.push_str(
        "\n\n\
         You are one bounded step of a job's automation in Forge. You have no tools: you cannot \
         read or write files, run commands, or reach the network. Decide from the inputs below \
         alone and return the structured object the schema you were given describes.\n\n",
    );
    p.push_str(&format!("This step: {}", action.description));
    if let Some(extra) = &action.prompt {
        p.push_str(&format!("\n{extra}"));
    }
    p
}

/// A directive job step's inputs as text: the trigger's input document and
/// every earlier step's output, bounded to `input_bytes`. The prompt's data
/// half, and the whole of what a `jev` runner is given as its state.
pub(super) fn directive_inputs(
    input_text: &str,
    step_outputs: &[(String, String)],
    input_bytes: usize,
) -> String {
    let mut inputs = format!("The input document:\n{input_text}");
    for (name, output) in step_outputs {
        inputs.push_str(&format!("\n\nThe output of step {name:?}:\n{output}"));
    }
    bounded(&inputs, input_bytes)
}

/// A directive job step's prompt (docs/JOBS.md, "Steps"): `directive_
/// instructions` followed by its inputs — the trigger's input document and
/// every earlier step's output, as text, bounded to `input_bytes`. What a
/// claude or codex runner, which take one prompt and have no system
/// channel of their own, are launched with in full; `Runner::Chat` gets
/// `directive_instructions` again as its own system message (some
/// duplication, since this already carries it) and this whole text as its
/// user message, so its behavior matches what the other two runners see.
pub(super) fn directive_prompt(
    action: &workflows::ActionDef,
    input_text: &str,
    step_outputs: &[(String, String)],
    input_bytes: usize,
) -> String {
    let inputs = directive_inputs(input_text, step_outputs, input_bytes);
    let mut p = directive_instructions(action);
    p.push_str(&format!("\n\n{inputs}"));
    p
}
