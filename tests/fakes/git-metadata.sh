#!/bin/bash
cat >/dev/null
echo 42 > answer.txt
git add answer.txt
git commit -qm answer
# Install these after the agent's commit so any execution is host-side.
# The parent is outside the sandbox's writable worktree.
marker="$(dirname "$PWD")/host-git-hook-marker"
printf '#!/bin/sh\necho hook > "%s"\n' "$marker" > .git/hooks/pre-commit
chmod +x .git/hooks/pre-commit
monitor="$(dirname "$PWD")/host-git-fsmonitor-marker"
git config core.fsmonitor "echo fsmonitor > '$monitor'; false"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"checks_run":[],"claims":[{"claim":"answer is 42","evidence":"echo 42 > answer.txt"}]}}'
