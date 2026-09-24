#!/bin/bash
# runs something, then demotes with a command that reproduces the defect
cat >/dev/null
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"npm test"}}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.02,"result":"demote","structured_output":{"schema_version":1,"summary":"found a defect","needs_input":{"question":"answer.txt is 42 but the task also asked for a trailing newline; `xxd answer.txt` shows none","kind":"review","options":[],"context":"","checkpoint":null},"changes":[],"checks_run":[{"check":"answer","passed":true}],"claims":[{"claim":"no trailing newline","evidence":"xxd answer.txt"}]}}'
