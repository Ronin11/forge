#!/bin/bash
# the concierge directive: the message asks about the record, which
# already shows the answer, so it is a question, answered in plain words.
cat >/dev/null
decision='{"kind":"question","task":"","answer":"Yes, the reminder task for the Hendersons landed this morning.","reason":"","question":""}'
escaped="${decision//\\/\\\\}"
escaped="${escaped//\"/\\\"}"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$escaped"'","needs_input":null,"changes":[],"checks_run":[],"claims":[]}}'
