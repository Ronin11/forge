#!/bin/bash
# writes the answer; when told a verification failed, also writes what it asked for
prompt=$(cat)
if grep -q 'verification failed after your change' <<<"$prompt"; then
  echo extra > extra.txt; git add -A && git commit -qm "extra"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"added extra","changes":[{"path":"extra.txt","kind":"added"}]}}'
else
  echo 42 > answer.txt; git add -A && git commit -qm "answer"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"answer","changes":[{"path":"answer.txt","kind":"added"}]}}'
fi
