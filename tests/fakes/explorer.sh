#!/bin/bash
# first run: runs the same grep five times and reads twenty-six files without editing
# anything, then would sleep for half a minute; Forge must stop it before that.
# resumed run: must be handed --resume with the session it announced, then does the work.
cat >/dev/null
resume=""
while [ $# -gt 0 ]; do [ "$1" = "--resume" ] && resume="$2"; shift; done
if [ -z "$resume" ]; then
  echo '{"type":"system","subtype":"init","session_id":"sess-explore-1"}'
  for i in 1 2 3 4 5; do
    echo '{"type":"assistant","message":{"id":"g'$i'","content":[{"type":"tool_use","id":"g'$i'","name":"Bash","input":{"command":"grep -rn answer ."}}]}}'
    echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"g'$i'","content":""}]}}'
  done
  for i in $(seq 1 26); do
    echo '{"type":"assistant","message":{"id":"r'$i'","content":[{"type":"tool_use","id":"r'$i'","name":"Read","input":{"file_path":"hello.sh"}}]}}'
    echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"r'$i'","content":"ok"}]}}'
  done
  sleep 30
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":40,"total_cost_usd":0.5,"result":"never","session_id":"sess-explore-1"}'
  exit 0
fi
test "$resume" = "sess-explore-1" || { echo "wrong session: $resume" >&2; exit 3; }
echo '{"type":"system","subtype":"init","session_id":"sess-explore-1"}'
echo 42 > answer.txt
git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.01,"result":"done","session_id":"sess-explore-1","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"finished after being stopped","changes":[{"path":"answer.txt","kind":"added"}]}}'
