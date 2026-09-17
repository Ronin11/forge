#!/bin/bash
# the concierge directive: none of the other three is safe to conclude,
# so it asks the one question that would tell it which.
cat >/dev/null
decision='{"kind":"unclear","task":"","answer":"","reason":"","question":"Do you want the price change applied to future quotes only, or to the one you already sent today too?"}'
escaped="${decision//\\/\\\\}"
escaped="${escaped//\"/\\\"}"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$escaped"'","needs_input":null,"changes":[],"checks_run":[],"claims":[]}}'
