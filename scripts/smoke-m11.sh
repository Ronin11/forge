#!/usr/bin/env bash
# M11 smoke: human loop, knowledge, hygiene (SMOKE spec §M11).
# Steps 1-4 run against the LIVE ~/.forge and spend budget (real claude);
# steps 5-7 are cheap API/CLI checks. Run from the repo root:
#   scripts/smoke-m11.sh [step...]
set -u
cd "$(dirname "$0")/.."
FORGE=./forge
LOG=.scratch/smoke-m11.log
mkdir -p .scratch
: > "$LOG"
say() { echo; echo "### $*" | tee -a "$LOG"; }
run() { echo "\$ $*" | tee -a "$LOG"; "$@" 2>&1 | tee -a "$LOG"; }

API=http://127.0.0.1:7340/api/v1

newest_task() { $FORGE task list | awk 'NR==2{print $1}'; }

newest_attempt() {
  $FORGE task show "$1" --json 2>/dev/null \
    | python3 -c 'import json,sys;a=json.load(sys.stdin).get("attempts",[]);print(a[-1]["id"] if a else "")'
}

target_state() {
  $FORGE task show "$1" --json 2>/dev/null \
    | python3 -c 'import json,sys;print(json.load(sys.stdin)["targets"][0]["state"])' 2>/dev/null
}

wait_state() { # wait_state <task-id> <state...>
  local id=$1; shift
  local st=""
  for _ in $(seq 1 60); do
    st=$(target_state "$id")
    for want in "$@"; do [ "$st" = "$want" ] && { echo "$st"; return; }; done
    sleep 5
  done
  echo "$st"
}

step1() {
  say "1. steer changes a live agent's course mid-run (visible in the transcript)"
  run $FORGE task add "Count slowly: print the numbers 1..30 one per turn, running 'sleep 5' with Bash between numbers. If a later user message tells you to stop, stop immediately and do what it says." \
      --repo equitizr --model haiku --autonomy auto
  ID=$(newest_task)
  sleep 45   # let it claim and start counting
  run $FORGE task show "$ID"
  run $FORGE task tell "$ID" "Stop counting now. Reply with exactly the phrase STEERED-OK in your summary and finish."
  st=$(wait_state "$ID" succeeded failed unverified cancelled)
  echo "final: $st" | tee -a "$LOG"
  AT=$(newest_attempt "$ID")
  OUT=$(ls -t ~/.forge/worker/output/ | head -1)
  run grep -c "STEERED-OK" ~/.forge/worker/output/"$OUT" || echo "STEERED-OK not found (steer failed?)" | tee -a "$LOG"
  # The audit trail: steer enqueued and delivered, and the worker's lifecycle event.
  curl -s "$API/journal?since=0&limit=500" | grep -o '"kind":"attempt.steer[a-z_.]*"' | sort | uniq -c | tee -a "$LOG"
  curl -s "$API/attempts/$AT/events?limit=500&lines=false" | grep -o 'steer.delivered\|steer.dropped' | sort | uniq -c | tee -a "$LOG"
}

step2() {
  say "2. a question raises a desktop notification (notify plugin sends it)"
  run $FORGE task add "Before doing anything else, ask the human one question via needs_input: chocolate or vanilla? Options chocolate, vanilla." \
      --repo equitizr --model haiku --autonomy ask
  ID=$(newest_task)
  st=$(wait_state "$ID" waiting_human failed)
  echo "state: $st" | tee -a "$LOG"
  # The notify plugin logs every send; the desktop popup itself is eyeballed.
  run grep -c "notify" ~/.forge/logs/plugin.notify.log || run ls ~/.forge/logs/
  run $FORGE task answer "$ID" "vanilla"
  wait_state "$ID" succeeded failed unverified > /dev/null
}

step3() {
  say "3. a brief exists for the repository and is injected; tokens_to_first_edit recorded"
  # An explore run maintains the brief note (preamble instructs it).
  run $FORGE task add "Describe this repository: architecture, build and test commands, conventions, hot files, gotchas." \
      --repo equitizr --mode explore --model haiku --autonomy auto --wait
  run curl -s "$API/kb/search?q=brief%20equitizr"
  # The next attempt on the repository carries the brief via --append-system-prompt;
  # its effect lands in facts.tokens_to_first_edit on edit-making runs.
  run $FORGE task add "Add one line ('smoke m11') to FORGE_SMOKE.txt and commit it." \
      --repo equitizr --model haiku --autonomy auto --wait
  ID=$(newest_task); AT=$(newest_attempt "$ID")
  curl -s "$API/attempts/$AT" | python3 -c 'import json,sys;f=json.load(sys.stdin).get("facts") or {};print("tokens_to_first_edit:",f.get("tokens_to_first_edit"))' | tee -a "$LOG"
}

step4() {
  say "4. a repository without forge.toml yields a doc proposal"
  H=~/.forge
  R=.scratch/m11-bare-repo
  rm -rf "$R"; mkdir -p "$R"
  git -C "$R" init -q
  printf '{"scripts":{"test":"true"}}\n' > "$R/package.json"
  git -C "$R" add -A; git -C "$R" commit -qm init
  # Registering on the fly (task add with a path) advertises it within one tick.
  run $FORGE task add "Read-only: say hi." --repo "$PWD/$R" --model haiku --autonomy auto
  sleep 35   # one worker registration tick
  run $FORGE proposal list
  $FORGE proposal list 2>/dev/null | grep -c "forge.toml" | tee -a "$LOG"
}

step5() {
  say "5. the fourth question at max_questions=3 fails with ask_budget_exhausted"
  echo "covered deterministically by 'go test ./internal/web' (TestAskBudgetExhausted);" | tee -a "$LOG"
  echo "a real run needs an agent that asks four times, which haiku will not do on demand reliably." | tee -a "$LOG"
  curl -s "$API/journal?since=0&limit=1000" | grep -c "question.budget_exhausted" | tee -a "$LOG" || true
}

step6() {
  say "6. a duplicate task add is refused (and journaled); --force overrides"
  P="Dedupe probe $(date +%s)"
  run $FORGE task add "$P" --repo equitizr --model haiku --autonomy auto
  $FORGE task add "$P" --repo equitizr --model haiku --autonomy auto; echo "duplicate exit=$? (want 1)" | tee -a "$LOG"
  run $FORGE task add "$P" --repo equitizr --model haiku --autonomy auto --force
  curl -s "$API/journal?since=0&limit=500" | grep -c "work.deduplicated" | tee -a "$LOG"
  # tidy up: cancel both probes
  for i in 1 2; do ID=$(newest_task); run $FORGE task cancel "$ID"; sleep 1; done
}

step7() {
  say "7. forge task retry round-trip (cancel -> retry -> fresh attempt)"
  run $FORGE task add "Read-only: reply with one short sentence after running 'sleep 60' with Bash." --repo equitizr --model haiku --autonomy auto
  ID=$(newest_task)
  sleep 20
  run $FORGE task cancel "$ID"
  st=$(wait_state "$ID" cancelled failed)
  echo "after cancel: $st" | tee -a "$LOG"
  run $FORGE task retry "$ID"
  st=$(wait_state "$ID" succeeded failed unverified cancelled)
  echo "after retry: $st" | tee -a "$LOG"
  run $FORGE task show "$ID"
}

steps=("$@"); [ ${#steps[@]} -eq 0 ] && steps=(1 2 3 4 5 6 7)
for s in "${steps[@]}"; do "step$s"; done
echo; echo "log: $LOG"
