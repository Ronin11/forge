#!/bin/bash
# an investigator whose plan names files that do not exist
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":4,"total_cost_usd":0.01,"result":"planned","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"Plan: edit src/answer/mod.rs to return 42 and wire it through lib/main.rs; prove it with tests/answer_test.rs and the repository check named answer in forge.toml.","changes":[]}}'
