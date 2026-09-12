#!/bin/bash
# commits nothing and asks the operator a question
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"blocked","needs_input":{"tried":"read the tree and the task; stopped before writing anything","question":"Which answer file: answer.txt or ANSWER.txt?","options":["answer.txt","ANSWER.txt"],"context":"","checkpoint":null},"changes":[],"checks_run":[],"claims":[]}}'
