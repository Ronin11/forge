#!/bin/bash
# adds extra.txt after a pause, so another task can land meanwhile
cat >/dev/null
sleep 3
echo extra > extra.txt; git add -A && git commit -qm "extra"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"added extra","changes":[{"path":"extra.txt","kind":"added"}]}}'
