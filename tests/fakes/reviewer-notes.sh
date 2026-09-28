#!/bin/bash
# writes its reproduction under the review notes path, commits it, and
# demotes with the command that runs it from there
cat >/dev/null
mkdir -p tests/review-notes/1
printf '#!/bin/bash\n# forge review repro\ntail -c 1 answer.txt | od -c\n' > tests/review-notes/1/repro.sh
git add -A && git commit -qm "review notes"
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"bash tests/review-notes/1/repro.sh"}}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.02,"result":"demote","structured_output":{"schema_version":1,"summary":"found a defect","needs_input":{"question":"answer.txt has no trailing newline: `bash tests/review-notes/1/repro.sh` prints 2 where the task asked for a newline","kind":"review","options":[],"context":"","checkpoint":null},"changes":[],"checks_run":[],"claims":[{"claim":"no trailing newline","evidence":"bash tests/review-notes/1/repro.sh"}]}}'
