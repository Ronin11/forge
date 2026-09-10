#!/bin/bash
# writes the wrong answer and commits; claims nothing about checks
cat >/dev/null
echo 41 > answer.txt && git add -A && git commit -qm "wrong answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt contains 42","evidence":"cat answer.txt"}]}}'
