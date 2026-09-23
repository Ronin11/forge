#!/bin/bash
# first run: reads a file, commits a step, and dies at the turn cap;
# continuation run: must NOT be handed --resume; it finishes
source "$(dirname "$0")/lib.sh"
cat >/dev/null
parse_resume "$@"
if [ -z "$resume" ] && [ ! -f step.txt ]; then
  session "sess-fresh-1"
  echo '{"type":"assistant","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":5000},"content":[{"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"src/notes.txt"}}]}}'
  echo step > step.txt
  git add -A && git commit -qm "first step of the work"
  echo '{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":30,"total_cost_usd":0.02,"result":"Reached max turns (30)","session_id":"sess-fresh-1"}'
  exit 1
fi
test -z "$resume" || { echo "unexpected --resume $resume" >&2; exit 3; }
session "sess-fresh-2"
echo 42 > answer.txt
git add -A && git commit -qm "answer"
result "finished in a fresh session" answer.txt:added
