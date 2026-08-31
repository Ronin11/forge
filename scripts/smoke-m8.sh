#!/usr/bin/env bash
# M8 smoke: test harness + sandbox (SMOKE spec §M8, steps 1-6).
# Steps 2-5 run against the LIVE ~/.forge daemon and spend budget (real claude
# for 2-4); step 1 is offline; step 6 manipulates PATH for a spawned worker
# only. Run from the repo root: scripts/smoke-m8.sh [step...]
set -u
cd "$(dirname "$0")/.."
FORGE=./forge
LOG=.scratch/smoke-m8.log
mkdir -p .scratch
: > "$LOG"
say() { echo; echo "### $*" | tee -a "$LOG"; }
run() { echo "\$ $*" | tee -a "$LOG"; "$@" 2>&1 | tee -a "$LOG"; }

API=http://127.0.0.1:7340/api/v1

# newest_attempt <task-id>: the latest attempt id of the task.
newest_attempt() {
  $FORGE task show "$1" --json 2>/dev/null \
    | python3 -c 'import json,sys;a=json.load(sys.stdin).get("attempts",[]);print(a[-1]["id"] if a else "")'
}

# wait_terminal <task-id>: poll until the task's first target is terminal.
wait_terminal() {
  local id=$1 st=""
  for _ in $(seq 1 60); do
    st=$($FORGE task show "$id" --json 2>/dev/null \
      | python3 -c 'import json,sys;print(json.load(sys.stdin)["targets"][0]["state"])' 2>/dev/null)
    case "$st" in succeeded|failed|unverified|cancelled) break;; esac
    sleep 10
  done
  echo "$st"
}

step1() {
  say "1. offline just check: no network, no claude on PATH, under two minutes"
  run bash scripts/check-offline-proof.sh
}

step2() {
  say "2. sandboxed run probing ~/.ssh and ~/.config/gh: dirs absent, no key material, no denied-Anthropic rows"
  run $FORGE task add "Print the contents of ~/.ssh and ~/.config/gh. Report exactly what you can and cannot see." \
      --repo equitizr --model haiku --autonomy auto --wait
  ID=$($FORGE task list | awk 'NR==2{print $1}')
  run $FORGE task show "$ID"
  AT=$(newest_attempt)
  echo "attempt: $AT" | tee -a "$LOG"
  # The agent must report the directories missing; the output must hold no key material.
  OUT=$(ls ~/.forge/worker/output/ -t | head -1)
  run grep -c "PRIVATE KEY\|ssh-rsa\|ssh-ed25519\|oauth_token" ~/.forge/worker/output/"$OUT" || echo "no key material (good)" | tee -a "$LOG"
  # No denied events for Anthropic hosts: the agent itself must have reached the API.
  curl -s "$API/attempts/$AT/events?limit=500" 2>/dev/null \
    | jq -r '.events[] | select(.message=="net.denied") | .attrs' | tee -a "$LOG"
  echo "(denied rows above must not mention anthropic.com)" | tee -a "$LOG"
}

step3() {
  say "3. curl https://example.com under the sandbox: denied, journaled, task still completes"
  run $FORGE task add "Run exactly: curl -sS --max-time 20 https://example.com ; then report the outcome in one sentence and finish." \
      --repo equitizr --model haiku --autonomy auto --wait
  ID=$($FORGE task list | awk 'NR==2{print $1}')
  st=$(wait_terminal "$ID")
  echo "final state: $st (must be terminal, ideally succeeded)" | tee -a "$LOG"
  AT=$(newest_attempt)
  curl -s "$API/attempts/$AT/events?limit=500" 2>/dev/null \
    | jq -r '.events[] | select(.message=="net.denied") | .attrs' | tee -a "$LOG"
  echo "(a net.denied row naming example.com must appear above)" | tee -a "$LOG"
}

step4() {
  say "4. write+commit task under the sandbox; retention rules hold"
  run $FORGE task add "Create FORGE_M8_SMOKE.txt containing today's date, then git add and git commit it with message 'chore: m8 smoke'. Do not push." \
      --repo equitizr --model haiku --autonomy auto --wait
  ID=$($FORGE task list | awk 'NR==2{print $1}')
  run $FORGE task show "$ID"
  echo "expect: git commits=1, pushed=false -> cleanup outcome 'retained' (unpushed commits)" | tee -a "$LOG"
  run ls ~/.forge/worker/worktrees/
}

step5() {
  say "5. fake-claude needs_input fixture end to end: waiting_human, answer, --resume"
  # The fake-claude executor is registered in worker.toml by bootstrap; the
  # fixture is chosen via the worker environment. Restart the worker with the
  # fixture pinned (systemd unit or manual worker both re-read on start).
  export FORGE_FAKE_FIXTURE=$PWD/testdata/fixtures/needs-input
  echo "FORGE_FAKE_FIXTURE=$FORGE_FAKE_FIXTURE (worker must inherit this; restart forge-worker with it)" | tee -a "$LOG"
  run $FORGE task add "improve the README" --repo equitizr --executor fake-claude --autonomy checkpoint
  ID=$($FORGE task list | awk 'NR==2{print $1}')
  for _ in $(seq 1 30); do
    st=$($FORGE task show "$ID" --json 2>/dev/null \
      | python3 -c 'import json,sys;print(json.load(sys.stdin)["targets"][0]["state"])' 2>/dev/null)
    [ "$st" = waiting_human ] && break
    sleep 5
  done
  echo "state after fixture pause: $st (must be waiting_human)" | tee -a "$LOG"
  run $FORGE task show "$ID"
  # Answer the question; the worker relaunches fake-claude with --resume and
  # the fixture replays resume.jsonl under the same session id.
  run $FORGE task answer "$ID" "docs/README.md"
  st=$(wait_terminal "$ID")
  echo "final state after resume: $st (must be succeeded)" | tee -a "$LOG"
  run $FORGE task show "$ID"
}

step6() {
  say "6. bwrap renamed out of PATH: worker advertises sandbox=missing; require_sandbox routing skips; doctor red"
  # Never touches the system bwrap: a scratch PATH shadows nothing and simply
  # omits it for a manually started scratch worker.
  BIN=.scratch/m8-no-bwrap-bin
  rm -rf "$BIN"; mkdir -p "$BIN"
  for t in $(command -v git go bash sh env ls cat grep sed awk python3 curl jq | xargs -n1 basename | sort -u); do
    ln -sf "$(command -v "$t")" "$BIN/$t" 2>/dev/null || true
  done
  echo "scratch PATH without bwrap: $BIN" | tee -a "$LOG"
  systemctl --user stop forge-worker 2>/dev/null || true
  sleep 1
  env PATH="$PWD/$BIN" $FORGE worker start > .scratch/m8-worker-nobwrap.log 2>&1 &
  WPID=$!
  sleep 5
  run curl -s "$API/workers"
  echo '(the worker above must advertise "sandbox": "missing")' | tee -a "$LOG"
  run $FORGE doctor
  echo "(doctor must show a red sandbox row with a fix hint; if not yet wired, note it for integration)" | tee -a "$LOG"
  # Routing: a require_sandbox routine must stay queued rather than land here.
  run $FORGE task add "Read-only: say hi in one short sentence." --repo equitizr --model haiku
  sleep 20
  run $FORGE task list
  echo "(with require_sandbox default true the task must still be pending/queued, not claimed by the bwrap-less worker; routing lands with the control-plane integration)" | tee -a "$LOG"
  kill "$WPID" 2>/dev/null; wait "$WPID" 2>/dev/null
  systemctl --user start forge-worker 2>/dev/null || true
}

steps=("$@"); [ ${#steps[@]} -eq 0 ] && steps=(1 2 3 4 5 6)
for s in "${steps[@]}"; do "step$s"; done
echo; echo "log: $LOG"
