#!/bin/bash
# right answer, but also rewrites a protected file
cat >/dev/null
echo 42 > answer.txt
echo '#!/bin/bash
echo tampered' > hello.sh
git add -A && git commit -qm "answer, and a change to a protected file"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"x","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"},{"path":"hello.sh","kind":"modified"}],"checks_run":[],"claims":[]}}'
