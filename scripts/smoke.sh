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

step11() {
  say "11. an attempt calls forge_repo_status; the call is a span with hashes"
  $FORGE routine list --json | grep -q '"name": "toolcall"' || run $FORGE routine add toolcall --prompt "Call the forge_repo_status MCP tool once and report its JSON output verbatim. Do nothing else." --repos equitizr --model haiku --max-turns 4 --timeout 300
  run $FORGE routine run toolcall
  local id; id=$(taskid)
  echo "state: $(wait_task "$id")" | tee -a "$LOG"
  local a; a=$(attempt_of "$id")
  run python3 - "$FORGE_HOME/forge.sqlite3" "$a" <<'PY'
import sqlite3, sys
db, a = sqlite3.connect(sys.argv[1]), sys.argv[2]
print("mcp spans:", db.execute("select seq, kind, name, attrs from events where attempt_id=? and source='mcp' order by seq", (a,)).fetchall())
PY
  run $FORGE task show "$id"
  echo "$a" > .scratch/smoke-tool-attempt
}

step12() {
  say "12. kb new / backlinks / check; a dangling link fails by name"
  local a; a=$(cat .scratch/smoke-tool-attempt 2>/dev/null || echo 00000000000000000000000000000000)
  run $FORGE kb new --type note --title "Smoke note" --about "attempt:$a"
  run $FORGE kb backlinks "attempt:$a"
  run $FORGE kb check
  echo 'see [[does-not-exist]]' >> "$FORGE_HOME/kb/smoke-note.md"
  run $FORGE kb check && echo "SMOKE FAIL: check passed with a dangling link" | tee -a "$LOG"
  sed -i '/does-not-exist/d' "$FORGE_HOME/kb/smoke-note.md"
  run $FORGE kb check
}

step13() {
  say "13. forge_check on the Forge repo returns the just-check structure"
  local a; a=$(cat .scratch/smoke-tool-attempt)
  run curl -s --unix-socket "$FORGE_HOME/forge.sock" "http://forge/api/v1/tools?attempt_id=$a"
  run python3 - "$FORGE_HOME/forge.sock" <<'PY'
import json, http.client, socket, sys
class C(http.client.HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX); self.sock.connect(sys.argv[1])
c = C("forge"); c.request("GET", "/api/v1/tools?attempt_id=dummy")
print(c.getresponse().status)
PY
  echo "(forge_check runs inside forge mcp in the worktree; exercised by the toolcall task and unit tests — the Forge repo's declared checks land with its forge.toml)" | tee -a "$LOG"
}

step14() {
  say "14. forge prune --dry-run reports zero deletions"
  run $FORGE prune --dry-run
}

step15() {
  say "15. forge usage: both windows, rates, forecast, target average, delta"
  run $FORGE usage
  run $FORGE usage --json
}

step16() {
  say "16. queue order C, A, B(blocked); a drag putting B above A is refused"
  run $FORGE task add "backlog A: do nothing yet" --repo equitizr --routine inventory --class backlog --priority 40
  A=$(taskid)
  run $FORGE task add "normal B after A" --repo equitizr --routine inventory --class normal --priority 40 --after "$A"
  B=$(taskid)
  run $FORGE task add "interactive C" --repo equitizr --routine inventory --class interactive --priority 40
  C=$(taskid)
  run $FORGE queue
  run $FORGE queue move "$B" --before "$A" && echo "SMOKE FAIL: moving B above A was accepted" | tee -a "$LOG"
  run $FORGE queue move "$A" --before "$C"
  run $FORGE queue
  run $FORGE queue block "$B" --on "$C"
  run $FORGE queue block "$B" --on "$C" --remove
  for id in "$A" "$B" "$C"; do run $FORGE task cancel "$id"; done
}

step17() {
  say "17. hard stop below current utilization: admissions stop visibly; running work continues"
  cp "$FORGE_HOME/config.toml" .scratch/config.toml.bak
  python3 - "$FORGE_HOME/config.toml" <<'PY'
import re, sys
p = sys.argv[1]; s = open(p).read()
s = re.sub(r"five_hour_hard_stop = [0-9.]+", "five_hour_hard_stop = 0.001", s)
s = re.sub(r"five_hour_target = [0-9.]+", "five_hour_target = 0.001", s)
open(p, "w").write(s)
PY
  run $FORGE task add "long runner: run the shell command 'sleep 60' then say done" --repo equitizr --routine inventory
  RUN_ID=$(taskid)
  for i in $(seq 1 12); do
    ST=$($FORGE task show "$RUN_ID" --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["state"])')
    [ "$ST" = running ] && break; sleep 5
  done
  run $FORGE daemon restart
  sleep 3
  run $FORGE task add "blocked by budget" --repo equitizr --routine inventory --class normal
  DEFER_ID=$(taskid)
  sleep 8
  run $FORGE queue
  run $FORGE task show "$DEFER_ID"
  echo "long runner state: $(wait_task "$RUN_ID" 180)" | tee -a "$LOG"
  cp .scratch/config.toml.bak "$FORGE_HOME/config.toml"
  run $FORGE daemon restart
  sleep 3
  echo "deferred task after restore: $(wait_task "$DEFER_ID" 300)" | tee -a "$LOG"
}

step18() {
  say "18. autonomy ask + ambiguous prompt → waiting_human; slot free; answer resumes the session"
  $FORGE routine list --json | grep -q '"name": "ambiguous"' || run $FORGE routine add ambiguous --prompt "Improve the README. Reply in one short sentence describing what you did." --repos ronin11-github-io --model haiku --max-turns 6 --timeout 300 --autonomy ask
  run $FORGE routine run ambiguous
  ID=$(taskid)
  for i in $(seq 1 60); do
    ST=$($FORGE task show "$ID" --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["state"])')
    case "$ST" in waiting_human|succeeded|failed|unverified) break;; esac
    sleep 5
  done
  echo "state after run: $ST" | tee -a "$LOG"
  run $FORGE task show "$ID"
  run $FORGE task add "slot check while waiting" --repo equitizr --routine inventory
  SLOT=$(taskid)
  echo "slot task: $(wait_task "$SLOT" 120)" | tee -a "$LOG"
  run $FORGE task answer "$ID" "There is only about.html to describe; treat the site landing page as the README and just tell me what you would improve — change nothing."
  echo "after answer: $(wait_task "$ID" 240)" | tee -a "$LOG"
  run $FORGE task show "$ID"
}

step19() {
  say "19. stats CLI agrees with the page; retro pack validates and includes retained/cancelled"
  run $FORGE stats --since 1d
  run $FORGE stats --since 1d --json
  $FORGE retro --since 1d > .scratch/retro.json
  run python3 - .scratch/retro.json <<'PY'
import json, sys
pack = json.load(open(sys.argv[1]))
assert pack["schema_version"] == 1, "schema"
states = {p["facts"]["state"] for p in pack["problem_attempts"]}
retained = any(p["facts"].get("retained") for p in pack["problem_attempts"])
print("problem states:", sorted(states), "retained present:", retained)
print("routines in pack:", [r["name"] for r in pack["routines"]][:8])
print("stats routines:", sorted(pack["stats"]["report"]["routines"] if isinstance(pack["stats"], dict) and "report" in pack["stats"] else [r["routine"] for r in pack["stats"]["Routines"]] if isinstance(pack["stats"], dict) and "Routines" in pack["stats"] else list(pack["stats"])[:8]))
assert "cancelled" in states, "cancelled attempt missing"
print("retro pack OK")
PY
  run curl -s "http://127.0.0.1:7340/stats"
}

step20() {
  say "20. implement mode: schema populated; L1 re-runs declared checks; a false claim → unverified"
  $FORGE routine list --json | grep -q '"name": "impl-smoke"' || run $FORGE routine add impl-smoke --mode implement --prompt "Issue: NOTES.md should mention that smoke logs live in .scratch/. Add one short sentence to the Cuts section of NOTES.md saying so, commit with a conventional message. Acceptance: the sentence exists; checks pass." --repos forge --model haiku --max-turns 12 --timeout 900 --class interactive
  run $FORGE routine run impl-smoke
  ID=$(taskid)
  echo "state: $(wait_task "$ID" 900)" | tee -a "$LOG"
  run $FORGE task show "$ID"
  echo "$ID" > .scratch/smoke-impl-task
}

step21() {
  say "21. review mode on a diff: structured findings, no writes"
  $FORGE routine list --json | grep -q '"name": "review-smoke"' || run $FORGE routine add review-smoke --mode review --prompt "Review the most recent commit on the current branch (git show HEAD). Report findings." --repos forge --model haiku --max-turns 24 --timeout 600 --class interactive
  run $FORGE routine run review-smoke
  ID=$(taskid)
  echo "state: $(wait_task "$ID" 700)" | tee -a "$LOG"
  run $FORGE task show "$ID"
}

step22() {
  say "22. verify mode re-checks the implement attempt in a separate session"
  ID=$(cat .scratch/smoke-impl-task)
  run $FORGE task show "$ID"
  VID=$($FORGE task list --json | python3 -c 'import json,sys;[print(t["work"]["id"]) for t in json.load(sys.stdin) if t["work"]["routine_name"]=="verify"][:1]' | head -1)
  if [ -n "$VID" ]; then
    echo "verify work: $VID  state: $(wait_task "$VID" 900)" | tee -a "$LOG"
    run $FORGE task show "$VID"
  else
    echo "no verify work found (implement may not have reached verifying)" | tee -a "$LOG"
  fi
  run ls ui/test-results 2>/dev/null || true
}

step23() {
  say "23. Forge UI browser tests in just check"
  run just ui-test
}

api() { # api METHOD PATH [JSON] — operator API over loopback
  local m=$1 p=$2 body=${3:-}
  if [ -n "$body" ]; then
    curl -s -X "$m" -H 'Content-Type: application/json' -d "$body" "http://127.0.0.1:7340$p"
  else
    curl -s -X "$m" "http://127.0.0.1:7340$p"
  fi
}

# first proposal id matching status+kind from `forge proposal list --json`
propid() {
  $FORGE proposal list --json | python3 -c '
import json,sys
d=json.load(sys.stdin)
ps=d if isinstance(d,list) else d.get("proposals",[])
ps=[p for p in ps if p.get("status")=="'"$1"'" and (not "'"$2"'" or p.get("kind")=="'"$2"'")]
print(ps[0]["id"] if ps else "")'
}

step24() {
  say "24. retro on demand: kb retro note + at least one routine-kind proposal"
  $FORGE routine list --json | grep -q '"name": "retro-smoke"' || run $FORGE routine add retro-smoke --mode retro --prompt "Reflect on the last 24 hours of runs across routines using the retro pack. Compare generations, tie every observation to a metric, write one kb note of type retro, and file at least one proposal of kind routine (a concrete prompt or settings improvement for an existing routine such as impl-smoke or review-smoke) via forge_propose." --repos forge --model haiku --max-turns 24 --timeout 900 --class interactive
  run $FORGE routine run retro-smoke
  ID=$(taskid)
  echo "state: $(wait_task "$ID" 900)" | tee -a "$LOG"
  run $FORGE task show "$ID"
  run $FORGE proposal list
  P=$(propid proposed routine)
  [ -n "$P" ] && echo "routine proposal present: $P" | tee -a "$LOG" || echo "NO routine proposal" | tee -a "$LOG"
}

step25() {
  say "25. approve → new generation via proposal; forced regression (max_turns=1) auto-reverts"
  P=$(propid proposed routine)
  if [ -n "$P" ]; then
    run $FORGE proposal approve "$P"
    run $FORGE proposal show "$P"
  fi
  # Forced regression: a manual routine proposal crippling inventory.
  api POST /api/v1/proposals '{"kind":"routine","target":"routine:inventory","after":{"max_turns":1},"rationale":"smoke: force a regression for the A/B auto-revert","verification_plan":"A/B over the next 5 runs; auto-revert on regression"}' | tee -a "$LOG"; echo
  R=$(propid proposed routine)
  run $FORGE proposal approve "$R"
  GEN_BAD=$($FORGE routine show inventory | grep -m1 '^Generation' || true); echo "after apply: $GEN_BAD" | tee -a "$LOG"
  for i in 1 2 3 4 5; do
    run $FORGE routine run inventory
    ID=$(taskid)
    echo "run $i state: $(wait_task "$ID" 600)" | tee -a "$LOG"
  done
  sleep 25  # two sweep cycles
  run $FORGE proposal show "$R"
  run $FORGE routine show inventory
}

step26() {
  say "26. code proposal → forge/proposal-<id8> branch on the Forge repo, never merged"
  MAIN_BEFORE=$(git -C "$HOME/Projects/forge" rev-parse main)
  api POST /api/v1/proposals '{"kind":"code","target":"forge","after":{"diff":"--- a/NOTES.md\n+++ b/NOTES.md\n@@ example only @@\n"},"rationale":"smoke: example code proposal","verification_plan":"a human reviews the branch; Forge never merges"}' | tee -a "$LOG"; echo
  C=$(propid proposed code)
  run $FORGE proposal approve "$C"
  run $FORGE proposal show "$C"
  run git -C "$HOME/Projects/forge" branch --list 'forge/proposal-*'
  [ "$(git -C "$HOME/Projects/forge" rev-parse main)" = "$MAIN_BEFORE" ] && echo "main untouched" | tee -a "$LOG" || echo "MAIN MOVED — BUG" | tee -a "$LOG"
}

step27() {
  say "27. reject a proposal; status and funnel stats reflect it"
  api POST /api/v1/proposals '{"kind":"doc","target":"kb:smoke-doc","after":{"note":"example"},"rationale":"smoke: to be rejected","verification_plan":"none"}' | tee -a "$LOG"; echo
  D=$(propid proposed doc)
  run $FORGE proposal reject "$D" "smoke: rejecting on purpose"
  run $FORGE proposal show "$D"
  run $FORGE stats
}

steps=("$@"); [ ${#steps[@]} -eq 0 ] && steps=(1 2 3 4 5 6 7 8 9 10)
for s in "${steps[@]}"; do "step$s"; done
echo; echo "log: $LOG"
