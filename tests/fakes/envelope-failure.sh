#!/bin/bash
set -e
cat >/dev/null
echo 42 > answer.txt
git add answer.txt
git commit -qm answer
echo '{"type":"result","is_error":true,"subtype":"error_max_structured_output_retries","terminal_reason":"structured_output_retry_exhausted","num_turns":21}'
exit 1
