#!/bin/bash
# a docs task that also touches code
cat >/dev/null
echo "# notes" > NOTES.md; echo 42 > answer.txt; git add -A && git commit -qm "notes and answer"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"x","changes":[{"path":"NOTES.md","kind":"added"},{"path":"answer.txt","kind":"added"}]}}'
