#!/bin/bash
# A claude whose sandbox refreshes the login: the first launch finds the
# seeded refresh token r0, rewrites the credentials file with a CLI-shaped
# rotated pair (tagged r1) and a later expiresAt, and answers wrong. A
# launch that finds r1 seeded answers right. So a task passes only when the
# kernel wrote the refreshed pair back over the host file and seeded the
# next sandbox from it. See src/login.rs.
cat >/dev/null
f="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json"
# A body of at least 32 [A-Za-z0-9_-] characters, tagged so a launch (and
# the test asserting on it) can tell which rotation it is seeing.
oat() { printf 'sk-ant-oat01-%s%s' "$1" "$(printf '0%.0s' $(seq 1 $((32 - ${#1}))))"; }
ort() { printf 'sk-ant-ort01-%s%s' "$1" "$(printf '0%.0s' $(seq 1 $((32 - ${#1}))))"; }
if grep -q "$(ort r1)" "$f"; then
  echo 42 >answer.txt
else
  # Six hours out: later than the seed's, and within the day the kernel accepts.
  at=$(( $(date +%s) * 1000 + 6 * 3600 * 1000 ))
  printf '{"claudeAiOauth":{"accessToken":"%s","refreshToken":"%s","expiresAt":%s}}' "$(oat a1)" "$(ort r1)" "$at" >"$f.new" && mv "$f.new" "$f"
  echo 41 >answer.txt
fi
git add -A && git commit -qm "attempt"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt holds the answer for the login it was seeded with","evidence":"cat answer.txt"}]}}'
