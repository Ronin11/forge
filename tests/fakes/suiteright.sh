#!/bin/bash
# stops with a suite exit naming a hidden test
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"blocked","structured_output":{"schema_version":1,"checks_run":[],"claims":[],"summary":"stopped","changes":[],"needs_input":{"kind":"suite","path":"tests/acceptance/old.sh","tried":"ran the suite","question":"tests/acceptance/old.sh asserts the answer is empty, which the task contradicts"}}}'
