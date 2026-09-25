#!/bin/bash
# ok.sh, plus its own argv (argv_debug) in the log so a test can see the --model it was launched with
source "$(dirname "$0")/lib.sh"
argv_debug "$@"
# (body of ok.sh)
cat >/dev/null
[ -n "$FAKE_SLEEP" ] && sleep "${FAKE_SLEEP_SECS:-2}"
echo '{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"utilization":0.42,"resetsAt":1800000000},"seven_day":{"utilization":0.13,"resetsAt":1800500000}}}}'
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Write","input":{}}]}}'
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Write","input":{}}]}}'
echo 42 > answer.txt && git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"usage":{"input_tokens":123,"output_tokens":45,"cache_read_input_tokens":67,"cache_creation_input_tokens":8},"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt contains 42","evidence":"cat answer.txt"}]}}'
