#!/bin/bash
# the tests step: writes one acceptance test under the namespace that fails on base
cat >/dev/null
mkdir -p tests/acceptance
printf '#!/bin/bash\ngrep -qx 42 answer.txt\n' > tests/acceptance/answer.sh
git add -A && git commit -qm "acceptance test for the answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"Interface: a file answer.txt at the repo root whose entire content is the line 42 (no other lines). Trailing whitespace is not tolerated.","changes":[{"path":"tests/acceptance/answer.sh","kind":"added"}]}}'
