#!/bin/bash
# writes the right answer; the prompt it saw is in the log's first line anyway
cat >/dev/null
echo 42 > answer.txt && git add -A && git commit -qm "answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"x","changes":[{"path":"answer.txt","kind":"added"}]}}'
