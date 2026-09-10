#!/bin/bash
# a tests step whose test already passes on base: it specifies nothing
cat >/dev/null
mkdir -p tests/acceptance
printf '#!/bin/bash\ntrue\n' > tests/acceptance/trivial.sh
git add -A && git commit -qm "trivial test"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"Interface: nothing in particular, this test passes regardless of the implementation.","changes":[{"path":"tests/acceptance/trivial.sh","kind":"added"}]}}'
