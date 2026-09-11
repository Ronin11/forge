#!/bin/bash
# like feedbackcoder.sh, but the first pass takes a moment so main can move underneath it
prompt=$(cat)
if grep -qE 'verification fail(ed|s)' <<<"$prompt"; then
  echo extra > extra.txt; git add -A && git commit -qm "extra"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"added extra","changes":[{"path":"extra.txt","kind":"added"}]}}'
else
  sleep 2
  echo 42 > answer.txt; git add -A && git commit -qm "answer"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"answer","changes":[{"path":"answer.txt","kind":"added"}]}}'
fi
