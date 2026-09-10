#!/bin/bash
# a docs task that stays in scope
cat >/dev/null
echo "# notes" > NOTES.md; git add -A && git commit -qm "notes"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"x","changes":[{"path":"NOTES.md","kind":"added"}]}}'
