#!/bin/bash
# like ok.sh, but reports the 5h window at 95%, resetting FAKE_RESET_SECS (default 3) seconds from now;
# the test scales it with the host load so the worker reaches its hold first
cat >/dev/null
r=$(( $(date +%s) + ${FAKE_RESET_SECS:-3} ))
echo '{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"utilization":0.95,"resetsAt":'"$r"'},"seven_day":{"utilization":0.13,"resetsAt":1800500000}}}}'
echo 42 > answer.txt && git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"wrote the answer","changes":[{"path":"answer.txt","kind":"added"}],"claims":[{"claim":"answer.txt contains 42","evidence":"cat answer.txt"}]}}'
