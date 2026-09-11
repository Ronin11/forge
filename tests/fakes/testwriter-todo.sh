#!/bin/bash
# the tests step: its first test carries a TODO the repo's lint rejects; told so, it writes a clean one
prompt=$(cat)
mkdir -p tests/acceptance
if grep -q 'every error is inside your tests' <<<"$prompt"; then
  printf '#!/bin/bash\ngrep -qx 42 answer.txt\n' > tests/acceptance/answer.sh
else
  printf '#!/bin/bash\n# TODO tighten\ngrep -qx 42 answer.txt\n' > tests/acceptance/answer.sh
fi
git add -A && git commit -qm "acceptance test for the answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"Interface: a file answer.txt at the repo root whose entire content is the line 42 (no other lines). Trailing whitespace is not tolerated.","changes":[{"path":"tests/acceptance/answer.sh","kind":"added"}]}}'
