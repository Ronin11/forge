#!/bin/bash
# the supervisor accepts a plain question whose checks already passed
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.03,"result":"ruled","structured_output":{"action":"accept","reason":"the checks already passed on the committed tree and the question is moot","answer":"The tree already has answer.txt set to 42 and the repository'"'"'s checks passed on it; ANSWER.txt is not needed, so the branch lands as it is.","citations":["answer.txt","forge.toml"],"prerequisite":null}}'
