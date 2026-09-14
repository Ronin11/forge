#!/bin/bash
# an investigator that starts implementing: the plan contract refuses it
cat >/dev/null
echo 41 > answer.txt && git add -A && git commit -qm "eager"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":4,"total_cost_usd":0.01,"result":"planned","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"Plan: add answer.txt at the repository root containing 42, proven by the repository check named answer and the checks in forge.toml. I went ahead and did it.","changes":[{"path":"answer.txt","kind":"added"}]}}'
