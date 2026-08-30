#!/usr/bin/env bash
# M1 smoke (docs/SMOKE.md 1–10) against the real repositories with real Claude.
# Spends budget. Records every command and its output to .scratch/smoke-m1.log.
# Usage: scripts/smoke.sh [step...]   (default: all)
set -uo pipefail
cd "$(dirname "$0")/.."
LOG=.scratch/smoke-m1.log
: > "$LOG"
export FORGE_HOME="${FORGE_HOME:-$HOME/.forge}"
FORGE=./forge
just build >/dev/null

say()  { printf '\n### %s\n' "$*" | tee -a "$LOG"; }
run()  { printf '$ %s\n' "$*" | tee -a "$LOG"; "$@" 2>&1 | tee -a "$LOG"; return "${PIPESTATUS[0]}"; }
taskid() {
  local id
  id=$($FORGE task list --json | python3 -c 'import json,sys;print(json.load(sys.stdin)[0]["work"]["id"])' 2>/dev/null)
  if [ -z "$id" ]; then echo "SMOKE ABORT: no task id" | tee -a "$LOG"; exit 1; fi
  echo "$id"
}
wait_task() { # id [timeout]
  local id=$1 t=${2:-600} s
  for ((i=0; i<t; i+=5)); do
    s=$($FORGE task show "$id" --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["state"])')
    case "$s" in succeeded|failed|cancelled|partial|unverified|waiting_human) echo "$s"; return;; esac
    sleep 5
  done
  echo "timeout($s)"
}
attempt_of() { $FORGE task show "$1" --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["attempts"][0]["id"])'; }

step1() {
  say "1. init via bootstrap; register repositories; auto-start"
  run $FORGE daemon stop || true
  run $FORGE task list
  if ! grep -q 'repositories.equitizr' "$FORGE_HOME/worker.toml"; then
    cat >> "$FORGE_HOME/worker.toml" <<TOML

[repositories.equitizr]
path = "$HOME/Projects/equitizr"
base_branch = "master"

[repositories.ronin11-github-io]
path = "$HOME/Projects/ronin11.github.io"
base_branch = "master"

[repositories.forge]
path = "$HOME/Projects/forge"
base_branch = "main"
TOML
    sed -i 's/^max_concurrent = .*/max_concurrent = 2/' "$FORGE_HOME/worker.toml"
    run $FORGE daemon restart
    sleep 3
  fi
  run $FORGE daemon status
  run curl -s --unix-socket "$FORGE_HOME/forge.sock" http://forge/api/v1/repositories
  echo | tee -a "$LOG"
}

step2() {
  say "2. routine inventory on equitizr + ronin11-github-io (haiku, 5 turns, 5m)"
  $FORGE routine list --json | grep -q '"name": "inventory"' || run $FORGE routine add inventory --prompt "Read-only task: list the top-level files and describe this repository in two sentences. Do not create, modify, or delete any files." --repos equitizr,ronin11-github-io --model haiku --max-turns 5 --timeout 300
  run $FORGE routine run inventory
  local id; id=$(taskid)
  echo "state: $(wait_task "$id")" | tee -a "$LOG"
  run $FORGE task show "$id"
  run ls "$FORGE_HOME/worker/worktrees"
}

step3() {
  say "3. routine touch on equitizr → retained (unpushed commits)"
  $FORGE routine list --json | grep -q '"name": "touch"' || run $FORGE routine add touch --prompt "Create FORGE_SMOKE.txt containing the current date and commit it with message 'chore: forge smoke test'. Do not push." --repos equitizr --model haiku --max-turns 8 --timeout 300
  run $FORGE routine run touch
  local id; id=$(taskid)
  echo "state: $(wait_task "$id")" | tee -a "$LOG"
  run $FORGE task show "$id"
  run git -C "$HOME/Projects/equitizr" branch --list 'forge/*'
  run git -C "$HOME/Projects/equitizr" status --short
  run git -C "$HOME/Projects/equitizr" branch --show-current
  attempt_of "$id" > .scratch/smoke-touch-attempt
}

step4() {
  say "4. forge cleanup preview, then --confirm; delete the branch by hand"
  local a; a=$(cat .scratch/smoke-touch-attempt)
  run $FORGE cleanup "$a"
  run $FORGE cleanup "$a" --confirm
  run ls "$FORGE_HOME/worker/worktrees"
  run git -C "$HOME/Projects/equitizr" branch --list 'forge/*'
  local b; b=$(git -C "$HOME/Projects/equitizr" branch --list 'forge/touch-*' | tr -d ' *')
  [ -n "$b" ] && run git -C "$HOME/Projects/equitizr" branch -D $b
}

step5() {
  say "5. ronin11.github.io checkout unchanged"
  run git -C "$HOME/Projects/ronin11.github.io" status --short
}

step6() {
  say "6. sleep 120 task; cancel; process group gone"
  run $FORGE task add "Run the shell command 'sleep 120' and wait for it to finish, then say done." --repo equitizr --model haiku --routine inventory
  local id; id=$(taskid)
  for i in 1 2 3 4 5 6 7 8 9 10 11 12; do pgrep -f 'sleep 120' >/dev/null && break; sleep 5; done
  run pgrep -af 'sleep 120'
  run $FORGE task cancel "$id"
  echo "state: $(wait_task "$id" 120)" | tee -a "$LOG"
  sleep 3
  run pgrep -af 'sleep 120' || echo "(no sleep 120 processes)" | tee -a "$LOG"
  run $FORGE task show "$id"
}

step7() {
  say "7. kill -9 the worker mid-attempt; restart; reconcile"
  run $FORGE task add "Run the shell command 'sleep 90' and then say done." --repo equitizr --model haiku --routine inventory
  local id; id=$(taskid)
  for i in $(seq 1 12); do pgrep -f 'sleep 90' >/dev/null && break; sleep 5; done
  local wpid; wpid=$(python3 -c "import json;print(json.load(open('$FORGE_HOME/daemon.json'))['worker_pid'])")
  run kill -9 "$wpid"
  sleep 2
  run "$FORGE" worker start --log-level warn &
  WPID=$!
  sleep 8
  run grep -h reconcile "$FORGE_HOME/logs/worker.log" | tail -3
  echo "state: $(wait_task "$id" 200)" | tee -a "$LOG"
  run $FORGE task show "$id"
  run pgrep -af 'sleep 90' || echo "(no sleep 90 processes)" | tee -a "$LOG"
  kill "$WPID" 2>/dev/null; wait "$WPID" 2>/dev/null
  run $FORGE daemon restart
}

step8() {
  say "8. kill -9 the daemon mid-attempt; the completion is accepted after restart"
  run $FORGE task add "Run the shell command 'sleep 45' and then say done." --repo equitizr --model haiku --routine inventory
  local id; id=$(taskid)
  for i in $(seq 1 12); do pgrep -f 'sleep 45' >/dev/null && break; sleep 5; done
  local dpid; dpid=$(python3 -c "import json;print(json.load(open('$FORGE_HOME/daemon.json'))['pid'])")
  run kill -9 "$dpid"
  sleep 5
  run $FORGE task show "$id"
  echo "state: $(wait_task "$id" 300)" | tee -a "$LOG"
  run $FORGE task show "$id"
}

step9() {
  say "9. stop and restart idle; reconcile reports nothing"
  run $FORGE daemon stop
  run $FORGE task list
  sleep 4
  run grep -h '"msg":"reconcile"' "$FORGE_HOME/logs/worker.log" | tail -1
}

step10() {
  say "10. spans, facts, samples, prompt version for a succeeded attempt"
  local id; id=$($FORGE task list --json | python3 -c 'import json,sys;[print(t["work"]["id"]) for t in json.load(sys.stdin) if t["state"]=="succeeded"][:1]' | head -1)
  local a; a=$(attempt_of "$id")
  run curl -s --unix-socket "$FORGE_HOME/forge.sock" "http://forge/api/v1/attempts/$a/events" 
  run python3 - "$FORGE_HOME/forge.sqlite3" "$a" <<'PY'
import sqlite3, sys
db, a = sqlite3.connect(sys.argv[1]), sys.argv[2]
print("spans:", db.execute("select name, duration_us from events where attempt_id=? and kind='span_end' and parent_id='' order by elapsed_us", (a,)).fetchall())
print("tool spans:", db.execute("select name, attrs from events where attempt_id=? and kind='span_start' and parent_id like 'agent-%'", (a,)).fetchall()[:3])
print("facts:", db.execute("select agent_us, total_us, input_tokens, output_tokens, tool_calls_by_name, prompt_version_hash from attempt_facts where attempt_id=?", (a,)).fetchall())
print("rate_limit metric:", db.execute("select count(*) from events where attempt_id=? and kind='metric' and name='rate_limit'", (a,)).fetchall())
print("samples:", db.execute("select window, utilization from rate_limit_samples where source_attempt=?", (a,)).fetchall())
print("prompt version:", db.execute("select routine, generation, model from prompt_versions where hash=(select prompt_version_hash from attempts where id=?)", (a,)).fetchall())
PY
}

steps=("$@"); [ ${#steps[@]} -eq 0 ] && steps=(1 2 3 4 5 6 7 8 9 10)
for s in "${steps[@]}"; do "step$s"; done
echo; echo "log: $LOG"
