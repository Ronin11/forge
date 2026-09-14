#!/bin/bash
# the supervisor cites a file that does not exist: refused, escalated
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.05,"result":"ruled","structured_output":{"action":"answer","reason":"there is a convention file","answer":"Use ANSWER.txt as docs/CONVENTIONS.md says every artifact name is uppercase.","citations":["docs/CONVENTIONS.md"],"prerequisite":null}}'
