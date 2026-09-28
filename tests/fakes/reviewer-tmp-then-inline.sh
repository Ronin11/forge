#!/bin/bash
# demotes citing a script in its sandbox's /tmp; once asked to inline the
# reproduction, demotes again with the commands inline
prompt=$(cat)
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"python3 /tmp/forge1-review.py"}}]}}'
if grep -q 'Your demotion cannot stand as written' <<<"$prompt"; then
  q='answer.txt has no trailing newline: `tail -c 1 answer.txt | od -c` prints 2 where the task asked for a newline'
else
  q='The command python3 /tmp/forge1-review.py demonstrates that answer.txt has no trailing newline'
fi
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.02,"result":"demote","structured_output":{"schema_version":1,"summary":"found a defect","needs_input":{"question":"'"$q"'","kind":"review","options":[],"context":"","checkpoint":null},"changes":[],"checks_run":[{"check":"answer","passed":true}],"claims":[{"claim":"no trailing newline","evidence":"tail -c 1 answer.txt"}]}}'
