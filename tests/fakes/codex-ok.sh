#!/bin/bash
# A fake codex CLI: emits the --json event stream shapes agent.rs's codex
# parser expects (thread.started, a command execution, the final
# agent_message, and turn.completed usage), honors --output-schema by
# writing a result that parses as the envelope, and logs its own argv (via
# argv_debug, a type the parser only logs) for the tests to check the flags
# Forge built — a file of its own would either land inside the worktree
# (which the L0 changes-match-git check would then flag as an unreported
# change) or outside it, where a sandboxed run cannot write it back to the
# host at all.
source "$(dirname "$0")/lib.sh"

# The prompt is the last, positional argument: everything codex's own flags
# precede it, and it is arbitrary multi-line text that would need real JSON
# string escaping (newlines included) to survive round-tripping through
# argv_debug. The tests only ever check the flags, so it is left out.
argv_debug "${@:1:$#-1}"

codex_thread "codex-fake-sess-1"
codex_command "i0" "echo 42 > answer.txt && git commit"
echo 42 > answer.txt
git add -A && git commit -qm "answer"
codex_result "wrote 42" answer.txt:added
codex_usage 100 10 50 5
