#!/bin/bash
# emits one tool call then dies without a result frame
cat >/dev/null
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}'
exit 1
