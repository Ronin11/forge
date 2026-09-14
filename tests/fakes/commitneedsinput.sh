#!/bin/bash
# commits an answer, then asks the operator a question
cat >/dev/null
echo 42 > answer.txt && git add -A && git commit -qm "answer" >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"blocked","needs_input":{"tried":"wrote an answer; unsure whether ANSWER.txt is also needed","question":"Should ANSWER.txt also be written?","options":["yes","no"],"context":"","checkpoint":null},"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[]}}'
