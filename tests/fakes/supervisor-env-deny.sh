#!/bin/bash
# the supervisor denies the environment need it is shown
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"ruled","structured_output":{"action":"deny","reason":"nothing shows the build needs this host"}}'
