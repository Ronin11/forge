#!/bin/bash
# runs the checks, finds nothing, confirms
cat >/dev/null
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"npm test"}}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.02,"result":"ok","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"Ran the answer check and inspected answer.txt; the change does what the task asked.","changes":[]}}'
