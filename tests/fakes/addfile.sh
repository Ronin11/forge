#!/bin/bash
# adds extra.txt, whatever else the tree holds
cat >/dev/null
echo extra > extra.txt; git add -A && git commit -qm "extra"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"added extra","changes":[{"path":"extra.txt","kind":"added"}]}}'
