#!/bin/bash
# commits two real files but reports no changes at all: the kernel derives
# changes[] from git regardless, so this must still succeed
cat >/dev/null
echo 42 > answer.txt && echo extra > extra.txt && git add -A && git commit -qm "answer and extra"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[],"checks_run":[],"claims":[{"claim":"answer.txt contains 42","evidence":"cat answer.txt"}]}}'
