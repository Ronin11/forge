package modes

// This file holds what bootstrap and the worker executor read off the mode
// definitions: the preamble seeds and the tool-list conventions.
//
// Tool-list conventions (MODES.md "Tool naming"): AllowedTools entries name
// Claude built-ins as Claude names them (Bash(git diff:*) patterns allowed)
// and Forge tools bare (forge_usage); the executor adds the mcp__forge__
// prefix. A mode that must run with no built-in tools at all puts the
// sentinel SentinelNoBuiltins ("-builtins") as the FIRST element of its
// AllowedTools; the executor strips it and passes --tools "" alongside
// --allowedTools. Every mode's list includes forge_note_progress,
// forge_kb_search, and forge_usage; forge_ask is added by the executor when
// the autonomy in force allows questions, never listed here.

// SentinelNoBuiltins as the first AllowedTools element means "no Claude
// built-in tools": the executor maps it to --tools "" and drops it from the
// --allowedTools list. It starts with '-' so it can never collide with a real
// tool name.
const SentinelNoBuiltins = "-builtins"

// Builtins is the full Claude built-in tool set a repository-writing mode
// gets ("all built-ins" in MODES.md). Task is deliberately absent: a subagent
// would escape the mode's tool restrictions and budget attribution. The
// slice is fresh on every call so callers may append to it.
func Builtins() []string {
	return []string{
		"Bash", "Edit", "Glob", "Grep", "NotebookEdit", "Read",
		"TodoWrite", "WebFetch", "WebSearch", "Write",
	}
}

// Seeds returns mode name → embedded default preamble, for bootstrap to seed
// <home>/modes/<name>.md (DESIGN.md §1.3). The caller passes the generated
// All() list (package modes/all); it cannot be called here without importing
// this package's importers.
func Seeds(all []Mode) map[string]string {
	out := make(map[string]string, len(all))
	for _, m := range all {
		out[m.Name()] = m.Preamble()
	}
	return out
}
