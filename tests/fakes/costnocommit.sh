#!/bin/bash
# writes the answer but never commits it, in a named session: every attempt fails the checks and costs a cent
source "$(dirname "$0")/lib.sh"
cat >/dev/null
session "sess-wrong-1"
echo 42 > answer.txt
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","session_id":"sess-wrong-1","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[]}}'
