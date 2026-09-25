#!/bin/bash
# A job's directive step that picks an outcome its action does not list.
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.0015,"result":"triaged","structured_output":{"job":"fix the fence","outcome":"shrug"}}'
