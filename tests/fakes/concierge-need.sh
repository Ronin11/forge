#!/bin/bash
# the concierge directive: a symptom with a workflow underneath it, not
# already on the record, so it is a need worth an interview.
cat >/dev/null
decision='{"kind":"need","task":"","answer":"","reason":"Losing track of who was quoted is a workflow, not a one-off change; the interview should find out what it is.","question":""}'
escaped="${decision//\\/\\\\}"
escaped="${escaped//\"/\\\"}"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$escaped"'","needs_input":null,"changes":[],"checks_run":[],"claims":[]}}'
