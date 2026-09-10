#!/bin/bash
# wrong on the first attempt; right once told which L1 check failed
prompt=$(cat)
if grep -q 'failed verification' <<<"$prompt" && grep -q 'L1 answer (exit 1)' <<<"$prompt"; then
  echo 42 > answer.txt
else
  echo 41 > answer.txt
fi
git add -A && git commit -qm "attempt"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done"}'
