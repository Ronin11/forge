#!/bin/bash
# A fake copilot CLI: emits the --output-format json event shapes agent.rs's
# copilot parser expects. Forge runs copilot in two phases, told apart here
# by whether `--resume` is on the argv:
#
# - Phase one (no --resume): does the work — a bash tool call, a plain
#   final message — and honors the git identity Forge sets so its commit
#   lands; its result frame names the session.
# - Phase two (--resume <session>): answers with the structured envelope
#   inside a code fence, with no further tool calls.
#
# It logs its own argv (via argv_debug) for the tests to check the flags
# Forge built for each phase; the prompt, the last argument after `-p`, is
# left out the way the codex fakes leave theirs out.
source "$(dirname "$0")/lib.sh"

argv_debug "${@:1:$#-1}"

has_resume=0
for a in "$@"; do
  if [ "$a" = "--resume" ]; then
    has_resume=1
  fi
done

if [ "$has_resume" = "1" ]; then
  copilot_envelope "wrote 42" answer.txt:added
  copilot_result "copilot-fake-sess-1" 1
else
  copilot_tool "call_0" "echo 42 > answer.txt && git commit"
  echo 42 > answer.txt
  git add -A && git commit -qm "answer"
  copilot_message "wrote 42 to answer.txt"
  copilot_result "copilot-fake-sess-1" 1
fi
