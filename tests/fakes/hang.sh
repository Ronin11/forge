#!/bin/bash
# never finishes
cat >/dev/null
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}'
sleep 300
