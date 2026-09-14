#!/bin/bash
# first run: stops with a suite exit naming a visible test; next run: does the work
source "$(dirname "$0")/lib.sh"
cat >/dev/null
if [ ! -f "$(pwd)/.git/suite-seen" ]; then
  touch "$(pwd)/.git/suite-seen"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"blocked","structured_output":{"schema_version":1,"checks_run":[],"claims":[],"summary":"stopped","changes":[],"needs_input":{"kind":"suite","path":"tests/other.test.ts","tried":"read tests/other.test.ts and my own tests/acceptance/answer.sh","question":"tests/other.test.ts:3 asserts the answer is empty, which the task contradicts; see tests/acceptance/answer.sh"}}}'
  exit 0
fi
rm -f "$(pwd)/.git/suite-seen"
echo 42 > answer.txt && git add -A && git commit -qm "answer"
result "wrote the answer" answer.txt:added
