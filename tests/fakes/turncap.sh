#!/bin/bash
# first run: starts the work, leaves it uncommitted, and dies at the turn cap;
# resumed run: must be handed --resume with the session it announced, then finishes
source "$(dirname "$0")/lib.sh"
cat >/dev/null
parse_resume "$@"
if [ -z "$resume" ]; then
  session "sess-turncap-1"
  echo 42 > answer.txt
  echo '{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":30,"total_cost_usd":0.02,"result":"Reached max turns (30)","session_id":"sess-turncap-1"}'
  exit 1
fi
test "$resume" = "sess-turncap-1" || { echo "wrong session: $resume" >&2; exit 3; }
session "sess-turncap-1"
git add -A && git commit -qm "answer"
result "finished after resuming" answer.txt:added
