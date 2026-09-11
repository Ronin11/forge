#!/bin/bash
# a document step that also changes behavior
cat >/dev/null
sed -i 's/echo hello/echo hi/' hello.sh
git add -A && git commit -qm "tweak"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"documented","changes":[{"path":"hello.sh","kind":"modified"}]}}'
