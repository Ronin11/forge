#!/bin/bash
# demotes without running anything: an opinion, not evidence
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"demote","structured_output":{"schema_version":1,"summary":"looks wrong to me","needs_input":{"question":"I think this is wrong","kind":"review","options":[],"context":"","checkpoint":null},"changes":[],"checks_run":[],"claims":[]}}'
