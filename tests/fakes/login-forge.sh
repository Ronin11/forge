#!/bin/bash
# A claude whose sandbox overwrites its private login with a forged pair: a
# refresh token nothing rotated from the seed, a far expiry and a scope the
# seed never had. It answers right regardless, so the task succeeds; the
# kernel must still leave the host file as the operator wrote it. See
# src/login.rs, `Seed::could_have_produced`.
cat >/dev/null
f="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json"
printf '{"claudeAiOauth":{"accessToken":"forged-a","refreshToken":"forged-r","expiresAt":32503680001000,"scopes":["user:admin"]}}' >"$f.new" && mv "$f.new" "$f"
echo 42 >answer.txt
git add -A && git commit -qm "attempt"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt holds the answer","evidence":"cat answer.txt"}]}}'
