#!/bin/bash
set -e
cat >/dev/null
test -n "$CARGO_TARGET_DIR"
echo agent >> "$CARGO_TARGET_DIR/agent"
echo 42 > answer.txt
git add answer.txt
git commit -qm answer
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt contains 42","evidence":"cat answer.txt"}]}}'
