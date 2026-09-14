#!/bin/bash
# first run: explores, changes nothing, and dies at the turn cap;
# resumed run: must be handed --resume with the session it announced, then does the work
source "$(dirname "$0")/lib.sh"
cat >/dev/null
parse_resume "$@"
if [ -z "$resume" ]; then
  session "sess-empty-1"
  echo '{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":30,"total_cost_usd":0.02,"result":"Reached max turns (30)","session_id":"sess-empty-1"}'
  exit 1
fi
test "$resume" = "sess-empty-1" || { echo "wrong session: $resume" >&2; exit 3; }
session "sess-empty-1"
echo 42 > answer.txt
git add -A && git commit -qm "answer"
result "finished after resuming" answer.txt:added
