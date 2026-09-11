#!/bin/bash
# writes the number the task names; told the base moved, merges it and keeps its own answer
prompt=$(cat)
n=$(grep -oE 'write [0-9]+' <<<"$prompt" | head -1 | grep -oE '[0-9]+')
if grep -q 'conflicts in' <<<"$prompt"; then
  git merge forge/main >/dev/null 2>&1 || true
  echo "$n" > answer.txt; git add -A && git commit -qm "merge main, keep $n" >/dev/null
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"merged main and kept my answer","changes":[{"path":"answer.txt","kind":"modified"}]}}'
else
  echo "$n" > answer.txt; git add -A && git commit -qm "answer $n" >/dev/null
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"answer","changes":[{"path":"answer.txt","kind":"added"}]}}'
fi
