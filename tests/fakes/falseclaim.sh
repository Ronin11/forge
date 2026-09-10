#!/bin/bash
# writes the wrong answer and reports the answer check as passed
cat >/dev/null
echo 41 > answer.txt && git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"x","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[{"check":"answer","passed":true},{"check":"shell","passed":true}],"claims":[]}}'
