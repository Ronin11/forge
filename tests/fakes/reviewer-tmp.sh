#!/bin/bash
# demotes citing a script in its sandbox's /tmp, however often it is asked
cat >/dev/null
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"python3 /tmp/forge1-review.py"}}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.02,"result":"demote","structured_output":{"schema_version":1,"summary":"found a defect","needs_input":{"question":"The command python3 /tmp/forge1-review.py demonstrates that answer.txt has no trailing newline","kind":"review","options":[],"context":"","checkpoint":null},"changes":[],"checks_run":[],"claims":[{"claim":"no trailing newline","evidence":"python3 /tmp/forge1-review.py"}]}}'
