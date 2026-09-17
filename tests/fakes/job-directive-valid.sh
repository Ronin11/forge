#!/bin/bash
# A job's directive step: no tools, no git identity to honor, just a
# structured result that matches the caller's schema.
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.0015,"result":"extracted","structured_output":{"job":"fix the fence","price_hint":250}}'
