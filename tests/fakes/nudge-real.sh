#!/bin/bash
# first run: reports a one-letter non-question; resumed run: still stops,
# but this time with a real question, which stands as asked
source "$(dirname "$0")/lib.sh"
cat >/dev/null
parse_resume "$@"
if [ -z "$resume" ]; then
  session "sess-nudge-2"
  echo 42 > answer.txt && git add -A && git commit -qm "answer"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":49,"total_cost_usd":0.03,"result":"blocked","session_id":"sess-nudge-2","structured_output":{"schema_version":1,"summary":"blocked","needs_input":{"tried":"wrote the answer","question":"q","options":[],"context":"","checkpoint":null},"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[]}}'
  exit 0
fi
test "$resume" = "sess-nudge-2" || { echo "wrong session: $resume" >&2; exit 3; }
session "sess-nudge-2"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"blocked","session_id":"sess-nudge-2","structured_output":{"schema_version":1,"summary":"blocked again","needs_input":{"tried":"read the tree again after the nudge","question":"Which timezone should the report use?","options":[],"context":"","checkpoint":null},"changes":[],"checks_run":[],"claims":[]}}'
