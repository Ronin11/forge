#!/bin/bash
# runs something, finds nothing, and writes the approval into the demotion field
cat >/dev/null
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cat answer.txt"}}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.02,"result":"ok","structured_output":{"schema_version":1,"summary":"no defect","needs_input":{"question":"No defect found - approving. `cat answer.txt` prints 42 as the task asked.","kind":"review","tried":"cat answer.txt","options":[],"context":"","checkpoint":null},"changes":[],"checks_run":[{"check":"answer","passed":true}],"claims":[]}}'
