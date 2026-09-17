#!/bin/bash
# A job's directive step whose agent run fails after producing a result:
# no structured output, an error subtype, and a line on stderr — what a
# tool-less launch that never gets to call `StructuredOutput` looks like.
cat >/dev/null
echo "boom: something in the sandbox broke" >&2
echo '{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":1,"total_cost_usd":0.001,"result":"I could not complete this."}'
exit 1
