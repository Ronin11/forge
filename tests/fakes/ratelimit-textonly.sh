#!/bin/bash
# Refuses with "rate limit" in the result text and no rate_limit_event at
# all, the case docs/REVIEW-3.md item 1.1.4 names: a refusal recognized
# only from the text, so agent.rs must invent the hold itself. First run:
# refuses; next run: fine.
source "$(dirname "$0")/lib.sh"
cat >/dev/null
# Persist provider state outside the candidate index and working tree.
if [ ! -f .git/rl-seen ]; then
  touch .git/rl-seen
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":0,"total_cost_usd":0,"result":"You have hit your rate limit","session_id":"s"}'
  exit 1
fi
rm -f .git/rl-seen
echo 42 > answer.txt && git add -A && git commit -qm "answer"
result "wrote the answer" answer.txt:added
