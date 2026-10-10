#!/bin/bash
# commits the right answer but claims it without evidence: the attempt fails, the commit passes the checks, and it costs a cent
source "$(dirname "$0")/lib.sh"
cat >/dev/null
session "sess-ne-1"
echo 42 > answer.txt && git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","session_id":"sess-ne-1","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt contains 42","evidence":""}]}}'
