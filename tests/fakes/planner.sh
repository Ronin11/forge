#!/bin/bash
# the investigate step: reads, changes nothing, returns a plan naming real paths
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":4,"total_cost_usd":0.01,"result":"planned","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"Plan: add answer.txt at the repository root containing 42. Leave hello.sh as it is; it is unrelated. The proof is the repository check named answer, which reads answer.txt, plus the existing checks in forge.toml. One commit, one file.","changes":[]}}'
