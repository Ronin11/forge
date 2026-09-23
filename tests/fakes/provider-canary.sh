#!/bin/bash
# Drops a canary in the sandboxed claude config directory on its first run
# in a worktree and answers wrong; answers right only when the canary is
# already there. So within one task the retry succeeds (the private
# provider state lives as long as the task, which `--resume` needs), and a
# second task, whose worktree has its own state, fails on its first attempt
# (nothing reaches it from the first task). See src/sandbox.rs,
# `provider_state_dir`.
cat >/dev/null
mkdir -p "$HOME/.claude"
if [ -e "$HOME/.claude/canary" ]; then
  echo 42 >answer.txt
else
  echo hi >"$HOME/.claude/canary"
  echo 41 >answer.txt
fi
git add -A && git commit -qm "attempt"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt contains 42 once the canary is there","evidence":"cat answer.txt"}]}}'
