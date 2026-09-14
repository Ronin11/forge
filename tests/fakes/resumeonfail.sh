#!/bin/bash
# wrong on the first attempt, checks fail; must be resumed in the same
# session on the second attempt (--resume-on-failure), then gets it right
cat >/dev/null
resume=""
while [ $# -gt 0 ]; do [ "$1" = "--resume" ] && resume="$2"; shift; done
if [ -z "$resume" ]; then
  echo '{"type":"system","subtype":"init","session_id":"sess-resumeonfail-1"}'
  echo 41 > answer.txt
  git add -A && git commit -qm "wrong answer"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","session_id":"sess-resumeonfail-1","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"answered","changes":[{"path":"answer.txt","kind":"added"}]}}'
  exit 0
fi
test "$resume" = "sess-resumeonfail-1" || { echo "wrong session: $resume" >&2; exit 3; }
echo '{"type":"system","subtype":"init","session_id":"sess-resumeonfail-1"}'
echo 42 > answer.txt
git add -A && git commit -qm "fix"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","session_id":"sess-resumeonfail-1","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"fixed","changes":[{"path":"answer.txt","kind":"modified"}]}}'
