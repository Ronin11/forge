#!/bin/bash
# attempt 1: commits the right answer, then dies before reporting.
# attempt 2 (sees feedback): changes nothing and honestly reports no changes.
prompt=$(cat)
if grep -q 'previous attempt ended without a result' <<<"$prompt"; then
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"already committed last time","needs_input":null,"changes":[],"checks_run":[],"claims":[]}}'
else
  echo 42 > answer.txt && git add -A && git commit -qm "answer"
  exit 1
fi
