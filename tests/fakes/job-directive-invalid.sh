#!/bin/bash
# A job's directive step whose structured result does not match the
# caller's schema: `price_hint` must be a number, not a string.
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.0015,"result":"extracted","structured_output":{"job":"fix the fence","price_hint":"a lot"}}'
