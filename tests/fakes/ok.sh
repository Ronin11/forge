#!/bin/bash
# writes the right answer, commits, reports it honestly, and shows a rate-limit sample
cat >/dev/null
echo '{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"utilization":0.42,"resetsAt":1800000000},"seven_day":{"utilization":0.13,"resetsAt":1800500000}}}}'
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Write","input":{}}]}}'
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Write","input":{}}]}}'
echo 42 > answer.txt && git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt contains 42","evidence":"cat answer.txt"}]}}'
