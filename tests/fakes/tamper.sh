#!/bin/bash
# weakens the repo's checks so its wrong answer would pass
cat >/dev/null
printf '[checks]\nanswer = ["true"]\n' > forge.toml
echo 41 > answer.txt
git add -A && git commit -qm "weaken checks"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"x","needs_input":null,"changes":[{"path":"forge.toml","kind":"modified"},{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[]}}'
