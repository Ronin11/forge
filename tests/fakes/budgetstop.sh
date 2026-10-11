#!/bin/bash
# must be handed --max-budget-usd with all of a $5 budget left; commits the
# right answer, then the CLI stops the run at that budget without a result
source "$(dirname "$0")/lib.sh"
cat >/dev/null
budget=""
while [ $# -gt 0 ]; do
  [ "$1" = "--max-budget-usd" ] && budget="$2"
  shift
done
test "$budget" = "5.0000" || { echo "wrong --max-budget-usd: '$budget'" >&2; exit 3; }
session "sess-budget-1"
echo 42 > answer.txt && git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"error_max_budget_usd","terminal_reason":"budget_exhausted","is_error":true,"num_turns":3,"total_cost_usd":0.02,"errors":["Reached maximum budget ($5.0000)"],"session_id":"sess-budget-1"}'
exit 1
