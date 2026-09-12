#!/bin/bash
# an agent that says the workflow is wrong for the task
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"blocked","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"blocked","needs_input":{"tried":"read the tree and the task; stopped before writing anything","question":"This needs a browser e2e step; no workflow has one.","kind":"workflow","options":[],"context":"","checkpoint":null},"changes":[]}}'
