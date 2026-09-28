#!/bin/bash
# A claude that refreshes the host login when the kernel probes it (the
# probe is the only launch with --no-session-persistence): it writes r1 and
# notes the probe beside the file. An attempt answers right only when it was
# seeded with the refreshed r1. See src/agent/refusal.rs, `refresh_on_host`.
cat >/dev/null
f="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json"
case " $* " in
  *" --no-session-persistence "*)
    echo probe >>"$f.probes"
    printf '{"claudeAiOauth":{"accessToken":"a1","refreshToken":"r1","expiresAt":32503680000000}}' >"$f.new" && mv "$f.new" "$f"
    exit 0
    ;;
esac
if grep -q '"refreshToken":"r1"' "$f"; then echo 42 >answer.txt; else echo 41 >answer.txt; fi
git add -A && git commit -qm "attempt"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt holds the answer for the login it was seeded with","evidence":"cat answer.txt"}]}}'
