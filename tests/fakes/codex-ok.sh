#!/bin/bash
# A fake codex CLI: emits the --json event stream shapes agent.rs's codex
# parser expects. Forge now runs codex in two phases, so this fake answers
# each differently, told apart by whether `--output-schema` is on its argv:
#
# - Phase one (no --output-schema): does the work — thread.started, a
#   command execution, a plain (non-JSON) final agent_message — and honors
#   the git identity Forge sets so its commit lands.
# - Phase two (--output-schema present, from `exec resume <thread_id>`):
#   answers with the structured envelope the schema file describes, with no
#   further tool calls.
#
# It logs its own argv (via argv_debug, a type the parser only logs) for the
# tests to check the flags Forge built for each phase — a file of its own
# would either land inside the worktree (which the L0 changes-match-git
# check would then flag as an unreported change) or outside it, where a
# sandboxed run cannot write it back to the host at all.
source "$(dirname "$0")/lib.sh"

# The prompt is the last, positional argument: everything codex's own flags
# precede it, and it is arbitrary multi-line text that would need real JSON
# string escaping (newlines included) to survive round-tripping through
# argv_debug. The tests only ever check the flags, so it is left out.
argv_debug "${@:1:$#-1}"

has_output_schema=0
for a in "$@"; do
  if [ "$a" = "--output-schema" ]; then
    has_output_schema=1
  fi
done

if [ "$has_output_schema" = "1" ]; then
  codex_result "wrote 42" answer.txt:added
  codex_usage 20 0 8 0
else
  codex_thread "codex-fake-sess-1"
  codex_command "i0" "echo 42 > answer.txt && git commit"
  echo 42 > answer.txt
  git add -A && git commit -qm "answer"
  echo '{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"wrote 42 to answer.txt"}}'
  codex_usage 100 10 50 5
fi
