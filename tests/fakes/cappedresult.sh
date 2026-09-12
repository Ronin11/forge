#!/bin/bash
# reports hitting the turn cap but still returns a proper result: a wrong answer the first time, right the second
cat >/dev/null
if [ -f answer.txt ] && grep -qx 41 answer.txt; then
  echo 42 > answer.txt; git add -A && git commit -qm "fix"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.01,"result":"done","session_id":"sess-capped-2","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"fixed","changes":[{"path":"answer.txt","kind":"modified"}]}}'
  exit 0
fi
for a in "$@"; do [ "$a" = "--resume" ] && { echo "must not be resumed" >&2; exit 3; }; done
echo '{"type":"system","subtype":"init","session_id":"sess-capped-1"}'
echo 41 > answer.txt; git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":30,"total_cost_usd":0.01,"result":"done","session_id":"sess-capped-1","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"answered","changes":[{"path":"answer.txt","kind":"added"}]}}'
