#!/bin/bash
# A claude whose sandbox refreshes the login: the first launch finds the
# seeded refresh token r0, rewrites the credentials file with r1 and a later
# expiresAt, and answers wrong. A launch that finds r1 seeded answers right.
# So a task passes only when the kernel wrote the refreshed pair back over
# the host file and seeded the next sandbox from it. See src/login.rs.
cat >/dev/null
f="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json"
if grep -q '"refreshToken":"r1"' "$f"; then
  echo 42 >answer.txt
else
  printf '{"claudeAiOauth":{"accessToken":"a1","refreshToken":"r1","expiresAt":32503680001000}}' >"$f.new" && mv "$f.new" "$f"
  echo 41 >answer.txt
fi
git add -A && git commit -qm "attempt"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt holds the answer for the login it was seeded with","evidence":"cat answer.txt"}]}}'
