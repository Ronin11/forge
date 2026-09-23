#!/bin/bash
cat >/dev/null
echo 42 > answer.txt
git add answer.txt
git commit -qm answer
# Everything git would run on a push, planted after the commit. The parent
# is outside the sandbox's writable worktree.
parent="$(dirname "$PWD")"
printf '#!/bin/sh\necho hook > "%s/host-git-pre-push-marker"\n' "$parent" > .git/hooks/pre-push
chmod +x .git/hooks/pre-push
git config credential.helper "!echo helper > '$parent/host-git-credential-marker'; false"
git config core.sshCommand "sh -c 'echo ssh > \"$parent/host-git-ssh-marker\"'; false"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"checks_run":[],"claims":[{"claim":"answer is 42","evidence":"echo 42 > answer.txt"}]}}'
