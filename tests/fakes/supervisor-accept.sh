#!/bin/bash
# the supervisor sets aside a demotion that names no defect
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.03,"result":"ruled","structured_output":{"action":"accept","reason":"the reviewer approved in the demotion field","answer":"The demotion text says no defect was found and cites the answer file printing 42; the branch passed every check and should land as it is.","citations":["answer.txt","forge.toml"],"prerequisite":null}}'
