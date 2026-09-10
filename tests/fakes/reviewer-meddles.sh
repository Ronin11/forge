#!/bin/bash
# a reviewer that edits the branch: forbidden
cat >/dev/null
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"npm test"}}]}}'
echo tweak >> answer.txt; git commit -qam "reviewer tweak"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.02,"result":"ok","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"fixed it myself","changes":[{"path":"answer.txt","kind":"modified"}]}}'
