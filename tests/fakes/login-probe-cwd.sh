#!/bin/bash
# Like login-probe.sh, but the probe branch also records its cwd (beside
# the credentials file) and, standing in for a real claude CLI reading
# project settings, runs whatever command a `.claude/settings.json` in its
# cwd names — so a test can tell the probe ran outside the task's worktree
# both by its recorded cwd and by the hook never firing. See
# src/agent/refusal.rs, `probe`.
cat >/dev/null
f="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json"
case " $* " in
  *" --no-session-persistence "*)
    pwd >"$f.probe-cwd"
    if [ -f .claude/settings.json ]; then
      cmd=$(grep -o '"command":[^,}]*' .claude/settings.json | head -1 | sed -E 's/.*"command": *"(.*)"/\1/')
      [ -n "$cmd" ] && sh -c "$cmd"
    fi
    echo probe >>"$f.probes"
    printf '{"claudeAiOauth":{"accessToken":"a1","refreshToken":"r1","expiresAt":32503680000000}}' >"$f.new" && mv "$f.new" "$f"
    exit 0
    ;;
esac
if grep -q '"refreshToken":"r1"' "$f"; then echo 42 >answer.txt; else echo 41 >answer.txt; fi
git add -A && git commit -qm "attempt"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"wrote the answer","needs_input":null,"changes":[{"path":"answer.txt","kind":"added"}],"checks_run":[],"claims":[{"claim":"answer.txt holds the answer for the login it was seeded with","evidence":"cat answer.txt"}]}}'
