#!/bin/bash
# First attempt: drops a canary in the sandboxed claude config directory and
# answers wrong, forcing a retry. The retry only answers right if that
# canary is gone — proving one attempt's provider-state write never reaches
# the next (src/sandbox.rs, `discard_provider_state`).
prompt=$(cat)
mkdir -p "$HOME/.claude"
if grep -q 'failed verification' <<<"$prompt" && grep -q 'L1 answer (exit 1)' <<<"$prompt"; then
  if [ -e "$HOME/.claude/canary" ]; then
    echo 41 >answer.txt
  else
    echo 42 >answer.txt
  fi
else
  echo hi >"$HOME/.claude/canary"
  echo 41 >answer.txt
fi
git add -A && git commit -qm "attempt"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt contains 42 once the canary is gone","evidence":"cat answer.txt"}]}}'
