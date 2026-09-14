#!/bin/bash
# first run: explores, changes nothing, and dies at the turn cap;
# resumed run: must be handed --resume with the session it announced, then does the work
cat >/dev/null
resume=""
while [ $# -gt 0 ]; do [ "$1" = "--resume" ] && resume="$2"; shift; done
if [ -z "$resume" ]; then
  echo '{"type":"system","subtype":"init","session_id":"sess-empty-1"}'
  echo '{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":30,"total_cost_usd":0.02,"result":"Reached max turns (30)","session_id":"sess-empty-1"}'
  exit 1
fi
test "$resume" = "sess-empty-1" || { echo "wrong session: $resume" >&2; exit 3; }
echo '{"type":"system","subtype":"init","session_id":"sess-empty-1"}'
echo 42 > answer.txt
git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.01,"result":"done","session_id":"sess-empty-1","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"finished after resuming","changes":[{"path":"answer.txt","kind":"added"}]}}'
