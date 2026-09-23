#!/bin/bash
# first run: a context past the threshold, commits a step, and fails with an
# ordinary error well under the turn cap; second run: must NOT be handed
# --resume; it finishes
source "$(dirname "$0")/lib.sh"
cat >/dev/null
parse_resume "$@"
if [ -z "$resume" ] && [ ! -f step.txt ]; then
  session "sess-ctx-1"
  echo '{"type":"assistant","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":130001},"content":[{"type":"text","text":"working"}]}}'
  echo step > step.txt
  git add -A && git commit -qm "first step of the work"
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":3,"total_cost_usd":0.02,"result":"something broke","session_id":"sess-ctx-1"}'
  exit 1
fi
test -z "$resume" || { echo "unexpected --resume $resume" >&2; exit 3; }
session "sess-ctx-2"
echo 42 > answer.txt
git add -A && git commit -qm "answer"
result "finished in a fresh session" answer.txt:added
