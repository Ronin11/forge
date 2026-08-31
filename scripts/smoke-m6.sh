#!/usr/bin/env bash
# M6 smoke: daemon, thin CLI, first run (SMOKE.md §M6, steps 1-10).
# Runs against the LIVE ~/.forge except step 7 (scratch home, live daemon
# stopped around it because both would bind 127.0.0.1:7340).
set -u
cd "$(dirname "$0")/.."
FORGE=./forge
LOG=.scratch/smoke-m6.log
: > "$LOG"
say() { echo; echo "### $*" | tee -a "$LOG"; }
run() { echo "\$ $*" | tee -a "$LOG"; "$@" 2>&1 | tee -a "$LOG"; }

# pgrep pattern built by concatenation so this script never matches itself.
DPAT="forge ""daemon"

step1() {
  say "1. stopped daemon + CLI call auto-starts; daemon.json valid; worker child up"
  run $FORGE daemon stop || true
  rm -f ~/.forge/forge.sock
  run $FORGE task list
  run python3 -c 'import json;d=json.load(open("'"$HOME"'/.forge/daemon.json"));print("daemon.json ok:",d["pid"],d["version"],d["state"])'
  wpid=$(python3 -c 'import json;print(json.load(open("'"$HOME"'/.forge/daemon.json")).get("worker_pid",""))' 2>/dev/null || true)
  run pgrep -f "forge ""worker" | head -3
}

step2() {
  say "2. eight concurrent CLI calls, one daemon"
  run $FORGE daemon stop || true
  sleep 1
  for i in $(seq 8); do $FORGE task list > .scratch/m6-c$i.out 2>&1 & done
  wait
  ok=0; for i in $(seq 8); do grep -q "ID\|no tasks" .scratch/m6-c$i.out && ok=$((ok+1)); done
  echo "successful CLIs: $ok/8" | tee -a "$LOG"
  echo "daemons: $(pgrep -c -f "$DPAT")" | tee -a "$LOG"
}

step3() {
  say "3. stale socket file with no daemon: removed, clean start"
  run $FORGE daemon stop || true
  sleep 1
  python3 -c 'import socket,os; p=os.path.expanduser("~/.forge/forge.sock"); os.path.exists(p) and os.remove(p); s=socket.socket(socket.AF_UNIX); s.bind(p)'
  run $FORGE task list
}

step4() {
  say "4. version-bumped second binary prints mismatch, exits non-zero, daemon untouched"
  go build -ldflags "-X main.version=v999-smoke" -o .scratch/forge-v999 ./cmd/forge
  before=$(python3 -c 'import json;print(json.load(open("'"$HOME"'/.forge/daemon.json"))["pid"])')
  .scratch/forge-v999 task list; code=$?
  echo "exit=$code" | tee -a "$LOG"
  after=$(python3 -c 'import json;print(json.load(open("'"$HOME"'/.forge/daemon.json"))["pid"])')
  echo "daemon pid before=$before after=$after" | tee -a "$LOG"
}

step5() {
  say "5. drain restart during a running attempt; journal draining -> restarted"
  $FORGE routine list --json | grep -q '"name": "sleeper"' || run $FORGE routine add sleeper --mode run --prompt "First run exactly: sleep 150. Then reply with one short sentence. Do not create or modify files." --repos equitizr --model haiku --max-turns 4 --timeout 400 --class interactive
  run $FORGE routine run sleeper
  sleep 20   # let the attempt claim and enter the agent phase
  run $FORGE task list
  run $FORGE daemon restart
  run $FORGE daemon status
  ID=$($FORGE task list | awk 'NR==2{print $1}')
  for i in $(seq 1 40); do
    st=$($FORGE task show "$ID" --json 2>/dev/null | python3 -c 'import json,sys;print(json.load(sys.stdin)["targets"][0]["state"])' 2>/dev/null)
    [ "$st" = succeeded ] || [ "$st" = failed ] || [ "$st" = unverified ] && break
    sleep 10
  done
  echo "sleeper final: $st" | tee -a "$LOG"
  run $FORGE task show "$ID"
  curl -s "http://127.0.0.1:7340/api/v1/journal?since=0" 2>/dev/null | grep -o '"kind":"daemon.[a-z]*"' | sort | uniq -c | tee -a "$LOG" || true
}

step6() {
  say "6. kill -9 the daemon during an attempt; task show auto-starts; completion accepted"
  run $FORGE routine run sleeper
  sleep 20
  ID=$($FORGE task list | awk 'NR==2{print $1}')
  dpid=$(python3 -c 'import json;print(json.load(open("'"$HOME"'/.forge/daemon.json"))["pid"])')
  run kill -9 "$dpid"
  sleep 2
  run $FORGE task show "$ID"
  for i in $(seq 1 40); do
    st=$($FORGE task show "$ID" --json 2>/dev/null | python3 -c 'import json,sys;print(json.load(sys.stdin)["targets"][0]["state"])' 2>/dev/null)
    [ "$st" = succeeded ] || [ "$st" = failed ] || [ "$st" = unverified ] && break
    sleep 10
  done
  echo "post-kill final: $st" | tee -a "$LOG"
  run $FORGE task show "$ID"
}

step7() {
  say "7. fresh box: FORGE_HOME scratch home, task add end to end, no init"
  run $FORGE daemon stop || true
  sleep 1
  H=.scratch/m6home
  rm -rf "$H"; mkdir -p "$H"
  FORGE_HOME=$PWD/$H run env FORGE_HOME=$PWD/$H $FORGE task add "Read-only: say hi in one short sentence." --repo equitizr --model haiku --wait
  run ls "$H"
  env FORGE_HOME=$PWD/$H $FORGE daemon stop || true
  sleep 1
  run $FORGE task list   # live daemon back
}

step8() {
  say "8. init --yes --with-browser; doctor green; init --service; systemd owns auto-start"
  run $FORGE init --yes --with-browser
  run $FORGE daemon restart
  sleep 3
  run curl -s http://127.0.0.1:7340/api/v1/workers
  run $FORGE doctor
  run $FORGE daemon stop || true
  sleep 1
  run $FORGE init --yes --service
  run systemctl --user status forge --no-pager
  run systemctl --user stop forge forge-worker
  sleep 1
  run $FORGE task list       # should start via systemd, not spawn
  run systemctl --user is-active forge
}

step9() {
  say "9. doctor red rows: daemon stopped + token 644; then restore"
  systemctl --user stop forge forge-worker 2>/dev/null || true
  run $FORGE daemon stop || true
  chmod 644 ~/.forge/worker.token 2>/dev/null || chmod 644 ~/.forge/token 2>/dev/null || true
  $FORGE doctor; echo "doctor exit=$?" | tee -a "$LOG"
  $FORGE doctor 2>&1 | head -25 | tee -a "$LOG"
  chmod 600 ~/.forge/worker.token 2>/dev/null || chmod 600 ~/.forge/token 2>/dev/null || true
  run $FORGE task list
}

step10() {
  say "10. every command has --help; bad input exits non-zero with one line"
  for cmd in init doctor version task routine queue proposal usage stats retro prune cleanup kb daemon worker service; do
    $FORGE $cmd --help > /dev/null 2>&1; h=$?
    echo "help $cmd: exit=$h" | tee -a "$LOG"
  done
  $FORGE task show nonexistent-zzz > /dev/null 2>&1; echo "bad task show exit=$?" | tee -a "$LOG"
  $FORGE routine add "bad name!" > /dev/null 2>&1; echo "bad routine add exit=$?" | tee -a "$LOG"
  $FORGE nosuchcmd > /dev/null 2>&1; echo "unknown cmd exit=$?" | tee -a "$LOG"
}

steps=("$@"); [ ${#steps[@]} -eq 0 ] && steps=(1 2 3 4 5 6 7 8 9 10)
for s in "${steps[@]}"; do "step$s"; done
echo; echo "log: $LOG"
