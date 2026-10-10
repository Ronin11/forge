#!/bin/bash
# commits the right answer, then leaves an uncommitted file and dies at the turn cap, costing two cents
source "$(dirname "$0")/lib.sh"
cat >/dev/null
session "sess-cd-1"
echo 42 > answer.txt && git add -A && git commit -qm "answer"
echo draft > notes.txt
echo '{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":30,"total_cost_usd":0.02,"result":"Reached max turns (30)","session_id":"sess-cd-1"}'
exit 1
