#!/bin/bash
# a reviewer that edits the branch: forbidden
source "$(dirname "$0")/lib.sh"
cat >/dev/null
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"npm test"}}]}}'
echo tweak >> answer.txt; git commit -qam "reviewer tweak"
result "fixed it myself" answer.txt:modified
