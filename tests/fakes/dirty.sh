#!/bin/bash
# right answer, but leaves an untracked file behind
cat >/dev/null
echo 42 > answer.txt && git add answer.txt && git commit -qm "answer"
echo scratch > notes.tmp
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done"}'
