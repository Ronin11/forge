#!/bin/bash
# the concierge directive: a legitimately short decision (under 120
# characters), to prove it is exempt from plan-substantive like interview.
cat >/dev/null
decision='{"kind":"question","task":"","answer":"No.","reason":"","question":""}'
escaped="${decision//\\/\\\\}"
escaped="${escaped//\"/\\\"}"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$escaped"'","needs_input":null,"changes":[],"checks_run":[],"claims":[]}}'
