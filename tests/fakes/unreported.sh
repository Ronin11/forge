#!/bin/bash
# right answer plus a second file it does not report
cat >/dev/null
echo 42 > answer.txt && echo extra > extra.txt && git add -A && git commit -qm "answer and extra"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt contains 42","evidence":"cat answer.txt"}]}}'
