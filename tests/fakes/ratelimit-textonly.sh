#!/bin/bash
# Refuses with "rate limit" in the result text and no rate_limit_event at
# all, the case docs/REVIEW-3.md item 1.1.4 names: a refusal recognized
# only from the text, so agent.rs must invent the hold itself. First run:
# refuses; next run: fine.
source "$(dirname "$0")/lib.sh"
cat >/dev/null
if [ ! -f "$(pwd)/.rl-seen" ]; then
  touch "$(pwd)/.rl-seen"
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":0,"total_cost_usd":0,"result":"You have hit your rate limit","session_id":"s"}'
  exit 1
fi
rm -f "$(pwd)/.rl-seen"
echo 42 > answer.txt && git add -A && git commit -qm "answer"
result "wrote the answer" answer.txt:added
