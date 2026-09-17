#!/bin/bash
# the concierge directive: the message says exactly what to do, so it is
# a request, tidied into a task with the reason.
cat >/dev/null
decision='{"kind":"request","task":"Change the quote text to say usually same day, since the shop now turns quotes around same day.","answer":"","reason":"","question":""}'
escaped="${decision//\\/\\\\}"
escaped="${escaped//\"/\\\"}"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$escaped"'","needs_input":null,"changes":[],"checks_run":[],"claims":[]}}'
