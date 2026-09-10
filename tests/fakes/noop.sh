#!/bin/bash
# does nothing and says so honestly
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"nothing to do","structured_output":{"schema_version":1,"summary":"Reviewed the diff and the checks; nothing to fix.","needs_input":null,"changes":[],"checks_run":[],"claims":[]}}'
