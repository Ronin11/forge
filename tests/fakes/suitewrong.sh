#!/bin/bash
# first run: stops with a suite exit naming a visible test; next run: does the work
cat >/dev/null
if [ ! -f "$(pwd)/.git/suite-seen" ]; then
  touch "$(pwd)/.git/suite-seen"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"blocked","structured_output":{"schema_version":1,"checks_run":[],"claims":[],"summary":"stopped","changes":[],"needs_input":{"kind":"suite","tried":"read tests/other.test.ts","question":"tests/other.test.ts:3 asserts the answer is empty, which the task contradicts"}}}'
  exit 0
fi
rm -f "$(pwd)/.git/suite-seen"
echo 42 > answer.txt && git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"checks_run":[],"claims":[],"needs_input":null,"summary":"wrote the answer","changes":[{"path":"answer.txt","kind":"added"}]}}'
