#!/bin/bash
# right answer after two seconds, for parallelism tests
cat >/dev/null
sleep 2
echo 42 > answer.txt && git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done"}'
