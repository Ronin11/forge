#!/usr/bin/env bash
# M7 smoke: plugins and the Omarchy indicator (SMOKE.md §M7, steps 1-8).
# Runs against the LIVE ~/.forge. Where a step needs a human watching the bar
# (SMOKE.md 5-7 widget rendering), the commands still run but the check is the
# omarchy-shell journal plus the status file's state field.
set -u
cd "$(dirname "$0")/.."
FORGE=./forge
LOG=.scratch/smoke-m7.log
mkdir -p .scratch
: > "$LOG"
say() { echo; echo "### $*" | tee -a "$LOG"; }
run() { echo "\$ $*" | tee -a "$LOG"; "$@" 2>&1 | tee -a "$LOG"; }

STATUS="${XDG_STATE_HOME:-$HOME/.local/state}/forge/status.json"
API=http://127.0.0.1:7340/api/v1

status_field() { python3 -c 'import json,sys;print(json.load(open("'"$STATUS"'")).get(sys.argv[1],""))' "$1" 2>/dev/null; }
status_age() { python3 - <<EOF
import json, datetime
d = json.load(open("$STATUS"))
ts = datetime.datetime.fromisoformat(d["ts"].replace("Z", "+00:00"))
print(int((datetime.datetime.now(datetime.timezone.utc) - ts).total_seconds()))
EOF
}

step1() {
  say "1. plugin list shows both; enable status-file; file appears <=5s, updates <=1s after task add"
  run $FORGE plugin list
  run $FORGE plugin install status-file --force
  run $FORGE plugin enable status-file --yes
  rm -f "$STATUS"
  for i in $(seq 1 25); do [ -f "$STATUS" ] && break; sleep 0.2; done
  if [ -f "$STATUS" ]; then echo "status file appeared after ~$((i / 5)).$((i % 5 * 2))s" | tee -a "$LOG"; else echo "FAIL: no status file within 5s" | tee -a "$LOG"; fi
  run cat "$STATUS"
  before_ts=$(status_field ts)
  run $FORGE task add "Read-only: say hi in one short sentence." --repo equitizr --model haiku --autonomy auto
  for i in $(seq 1 15); do [ "$(status_field ts)" != "$before_ts" ] && break; sleep 0.1; done
  echo "file updated after ~$((i * 100))ms of task add (queued=$(status_field queued), state=$(status_field state))" | tee -a "$LOG"
}

step2() {
  say "2. kill status-file; daemon restarts it with backoff; cursor resumes; counts not missing"
  run pgrep -af forge-status-file
  run pkill -f forge-status-file
  # submit work while the plugin is down
  run $FORGE task add "Read-only: say hi again, one short sentence." --repo equitizr --model haiku --autonomy auto
  for i in $(seq 1 60); do pgrep -f forge-status-file > /dev/null && break; sleep 1; done
  echo "status-file back after ~${i}s" | tee -a "$LOG"
  sleep 6   # one heartbeat so the file reflects the queue again
  run cat "$STATUS"
  echo "queued=$(status_field queued) state=$(status_field state)" | tee -a "$LOG"
  run $FORGE plugin status status-file
}

step3() {
  say "3. events:read-only plugin token gets 403 on POST /api/v1/tasks; journaled"
  # The daemon mints the per-plugin token; find where it is surfaced.
  TOKEN=$($FORGE plugin status status-file --json 2>/dev/null | python3 -c 'import json,sys;print(json.load(sys.stdin).get("token",""))' 2>/dev/null)
  if [ -z "${TOKEN:-}" ]; then TOKEN=$(cat "$HOME"/.forge/plugins/status-file/*.token 2>/dev/null || find "$HOME/.forge" -maxdepth 3 -name '*status-file*token*' -exec cat {} \; 2>/dev/null | head -1); fi
  if [ -z "${TOKEN:-}" ]; then
    echo "NOTE: plugin token not found on disk; check 'forge plugin status' output above for how to read it" | tee -a "$LOG"
  else
    code=$(curl -s -o .scratch/m7-403.out -w '%{http_code}' --unix-socket "$HOME/.forge/forge.sock" \
      -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
      -d '{"prompt":"nope","repositories":["equitizr"]}' http://forge/api/v1/tasks)
    echo "POST /api/v1/tasks with plugin token: HTTP $code" | tee -a "$LOG"
    cat .scratch/m7-403.out | tee -a "$LOG"; echo | tee -a "$LOG"
  fi
  curl -s "$API/journal?since=0&limit=1000" | grep -o '"kind":"plugin\.[a-z_]*"' | sort | uniq -c | tee -a "$LOG"
}

step4() {
  say "4. third-party echo-tools plugin: enable, run-mode task calls the tool, span exists; remove"
  rm -rf "$HOME/.forge/plugins/echo-tools"
  run cp -r plugins/examples/echo-tools "$HOME/.forge/plugins/echo-tools"
  run $FORGE plugin enable echo-tools
  run $FORGE daemon restart   # tools aggregation picks up enabled plugins at start
  sleep 3
  cat > .scratch/m7-echo-routine.toml <<'EOF'
Mode = "run"
Prompt = "Call the ping tool provided by the echo-tools plugin (its name contains 'echo' and 'ping') with msg set to 'smoke', then reply with exactly the tool's output. Do not create, modify, or delete any files."
Repositories = ["equitizr"]
Model = "haiku"
MaxTurns = 6
TimeoutSeconds = 300
AllowedTools = ["echo-tools_ping", "echo_tools_ping"]
BudgetClass = "interactive"
EOF
  $FORGE routine list --json 2>/dev/null | grep -q '"name": "echo-smoke"' || run $FORGE routine add echo-smoke --from .scratch/m7-echo-routine.toml
  run $FORGE routine run echo-smoke
  ID=$($FORGE task list | awk 'NR==2{print $1}')
  for i in $(seq 1 40); do
    st=$($FORGE task show "$ID" --json 2>/dev/null | python3 -c 'import json,sys;print(json.load(sys.stdin)["targets"][0]["state"])' 2>/dev/null)
    case "$st" in succeeded|failed|unverified|cancelled) break ;; esac
    sleep 5
  done
  echo "echo-smoke final: ${st:-unknown}" | tee -a "$LOG"
  AID=$($FORGE task show "$ID" --json 2>/dev/null | python3 -c 'import json,sys;a=json.load(sys.stdin)["attempts"];print(a[-1]["id"] if a else "")')
  echo "attempt: $AID" | tee -a "$LOG"
  curl -s "$API/attempts/$AID/events?lines=false&limit=500" | grep -oE '"[a-z_"]*(name|tool)[a-z_"]*":"[^"]*echo[-_]tools[-_]ping[^"]*"' | sort | uniq -c | tee -a "$LOG"
  curl -s "$API/attempts/$AID/events?lines=false&limit=500" | grep -c 'echo[-_]tools[-_]ping' | tee -a "$LOG"
  run $FORGE plugin disable echo-tools
  run rm -rf "$HOME/.forge/plugins/echo-tools"
}

step5() {
  say "5. install omarchy-indicator; shell logs clean; state reflects idle -> working -> attention"
  run $FORGE plugin install omarchy-indicator
  run ls -la "$HOME/.config/omarchy/plugins/ronin.forge/"
  run ls "$HOME/.config/omarchy/shell.json"*
  grep -o '"id": *"ronin.forge"' "$HOME/.config/omarchy/shell.json" | tee -a "$LOG"
  sleep 3
  run journalctl --user -u omarchy-shell -n 30 --no-pager
  echo "state (expect idle when nothing runs): $(status_field state)" | tee -a "$LOG"
  run $FORGE task add "First run exactly: sleep 45. Then reply with one short sentence. Do not create or modify files." --repo equitizr --model haiku --autonomy auto --class interactive
  sleep 25
  echo "state (expect working): $(status_field state)" | tee -a "$LOG"
  run $FORGE task add "There are two READMEs in this repository; ask me which one to improve before doing anything. Improve nothing yet." --repo equitizr --model haiku --autonomy ask --class interactive
  for i in $(seq 1 30); do [ "$(status_field state)" = attention ] && break; sleep 5; done
  echo "state (expect attention once the question is raised): $(status_field state)" | tee -a "$LOG"
  run $FORGE task list
  QID=$($FORGE task list --json 2>/dev/null | python3 -c 'import json,sys
for w in json.load(sys.stdin):
    qs = w.get("questions") or []
    if qs: print(qs[0]["id"]); break' 2>/dev/null)
  [ -n "${QID:-}" ] && run $FORGE task answer "$QID" "Neither - stop here; this was a smoke test. Do not modify anything."
  echo "widget rendering itself needs eyes on the bar; journal above stands in for it" | tee -a "$LOG"
}

step6() {
  say "6. stop daemon; file goes stale within 30s (consumer treats ts>30s as down); restart"
  run $FORGE daemon stop
  sleep 32
  echo "status file age: $(status_age)s (>30 = stale, icon shows off)" | tee -a "$LOG"
  run $FORGE daemon start
  sleep 6
  echo "status file age after restart: $(status_age)s, state=$(status_field state)" | tee -a "$LOG"
}

step7() {
  say "7. uninstall omarchy-indicator restores shell.json and removes the dir; reinstall at the end"
  run $FORGE plugin uninstall omarchy-indicator
  run ls "$HOME/.config/omarchy/plugins/ronin.forge"
  grep -c '"id": *"ronin.forge"' "$HOME/.config/omarchy/shell.json" | tee -a "$LOG" || true
  run $FORGE plugin list
  run $FORGE plugin install omarchy-indicator   # keep it installed
}

step8() {
  say "8. forge doctor includes plugin rows and is green"
  run $FORGE doctor
}

steps=("$@"); [ ${#steps[@]} -eq 0 ] && steps=(1 2 3 4 5 6 7 8)
for s in "${steps[@]}"; do "step$s"; done
echo; echo "log: $LOG"
