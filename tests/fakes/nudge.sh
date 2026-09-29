#!/bin/bash
# first run: finishes the work but reports a one-letter non-question;
# resumed run: must be handed --resume with the announced session, and
# returns a real result instead
source "$(dirname "$0")/lib.sh"
cat >/dev/null
parse_resume "$@"
if [ -z "$resume" ]; then
  session "sess-nudge-1"
  echo 42 > answer.txt && git add -A && git commit -qm "answer"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":49,"total_cost_usd":0.03,"result":"blocked","session_id":"sess-nudge-1","structured_output":{"schema_version":1,"summary":"blocked","needs_input":{"tried":"wrote the answer","question":"q","options":[],"context":"","checkpoint":null},"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[]}}'
  exit 0
fi
test "$resume" = "sess-nudge-1" || { echo "wrong session: $resume" >&2; exit 3; }
session "sess-nudge-1"
result "finished after the nudge" answer.txt:added
