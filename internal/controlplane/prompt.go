package controlplane

import (
	"fmt"
	"strings"

	"forge/internal/model"
)

// renderPrompt assembles an attempt's prompt: the autonomy block, the routine
// prompt with {{repo}} substituted, and a small context block (DESIGN.md and
// MODES.md "Prompt assembly"; the mode preamble layer arrives in M4 and slots
// in above the autonomy block).
func renderPrompt(routinePrompt, repo string, autonomy model.Autonomy) string {
	var b strings.Builder
	if block := autonomyBlock(autonomy); block != "" {
		b.WriteString(block)
		b.WriteString("\n\n")
	}
	b.WriteString(strings.ReplaceAll(routinePrompt, "{{repo}}", repo))
	fmt.Fprintf(&b, "\n\nContext: repository %s. You are in an isolated git worktree; never push.", repo)
	return b.String()
}

// autonomyBlock is the per-level instruction (MODES.md). The needs_input
// envelope must be the entire final message and unfenced, because the worker
// takes a result that parses as a JSON object as the structured envelope.
func autonomyBlock(a model.Autonomy) string {
	const envelope = `end your final message with ONLY this JSON object (no code fences, no prose around it):
{"schema_version":1,"summary":"<one sentence on where you stopped>","needs_input":{"question":"<the question>","options":["<option>","<option>"],"context":"<what the answerer should know>","checkpoint":null},"changes":[],"checks_run":[],"claims":[]}
Your session will be resumed with the human's answer as the next message.`
	switch a {
	case model.AutonomyAsk:
		return "AUTONOMY: ask. When anything is ambiguous, before any irreversible step (a commit, deleting a file, changing a dependency), or if the task looks far larger than its description, do not guess — " + envelope
	case model.AutonomyCheckpoint:
		return "AUTONOMY: checkpoint. Decide small ambiguities yourself and proceed. Only when a declared checkpoint or a genuinely blocking ambiguity is reached, " + envelope
	case model.AutonomyNotify, model.AutonomyAuto:
		return "AUTONOMY: " + string(a) + ". Decide and proceed; never ask for input. State assumptions in your summary instead."
	}
	return ""
}
