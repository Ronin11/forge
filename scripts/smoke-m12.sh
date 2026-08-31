#!/usr/bin/env bash
# M12 smoke: resilience and evals (SMOKE spec §M12). Steps 1 and 5 talk to the
# LIVE ~/.forge daemon; steps 2-4 work only on temporary homes and copies.
# Nothing here spends budget: the eval step runs the fake-claude executor.
# Honesty notes:
#   - step 2 simulates the "migration on a corrupted db" case by corrupting a
#     COPY of the backup's database and proving the daemon refuses to start on
#     it while the backup archive stays intact; no real migration is pending.
#   - step 3 is a rollback dry run against a scratch home seeded by a healthy
#     daemon start; the live home is never touched.
#   - step 4 "two prompt versions" is simulated with two case sets where one
#     contains a failing fixture — the scoring difference is the point.
# Run from the repo root: scripts/smoke-m12.sh [step...]
set -u
cd "$(dirname "$0")/.."
FORGE=./forge
LOG=.scratch/smoke-m12.log
SCRATCH=.scratch/m12
mkdir -p .scratch "$SCRATCH"
: > "$LOG"
say() { echo; echo "### $*" | tee -a "$LOG"; }
run() { echo "\$ $*" | tee -a "$LOG"; "$@" 2>&1 | tee -a "$LOG"; }

step1() {
  say "1. backup -> restore into a temp home -> the restored daemon serves the same task list"
  run $FORGE backup --out "$PWD/$SCRATCH"
  ARCHIVE=$(ls -t "$SCRATCH"/forge-backup-*.tar.gz | head -1)
  echo "archive: $ARCHIVE" | tee -a "$LOG"
  LIVE_COUNT=$($FORGE task list --json | python3 -c 'import json,sys;print(len(json.load(sys.stdin)))')
  RHOME=$(mktemp -d /tmp/forge-m12-restore.XXXXXX)
  run env FORGE_HOME="$RHOME" $FORGE restore "$PWD/$ARCHIVE"
  # A different port so the live daemon keeps 7340; the restored config still
  # names the old one.
  env FORGE_HOME="$RHOME" FORGE_HTTP=127.0.0.1:7341 $FORGE daemon start --foreground \
    > "$SCRATCH/restored-daemon.log" 2>&1 &
  DPID=$!
  sleep 3
  RESTORED_COUNT=$(env FORGE_HOME="$RHOME" $FORGE task list --json | python3 -c 'import json,sys;print(len(json.load(sys.stdin)))')
  echo "live tasks: $LIVE_COUNT, restored tasks: $RESTORED_COUNT (must match)" | tee -a "$LOG"
  kill "$DPID" 2>/dev/null; wait "$DPID" 2>/dev/null
  [ "$LIVE_COUNT" = "$RESTORED_COUNT" ] && echo "step1: OK" | tee -a "$LOG" || echo "step1: MISMATCH" | tee -a "$LOG"
}

step2() {
  say "2. a corrupted database fails closed; the backup archive stays intact (simulated on a copy)"
  ARCHIVE=$(ls -t "$SCRATCH"/forge-backup-*.tar.gz 2>/dev/null | head -1)
  if [ -z "$ARCHIVE" ]; then run $FORGE backup --out "$PWD/$SCRATCH"; ARCHIVE=$(ls -t "$SCRATCH"/forge-backup-*.tar.gz | head -1); fi
  CHOME=$(mktemp -d /tmp/forge-m12-corrupt.XXXXXX)
  run env FORGE_HOME="$CHOME" $FORGE restore "$PWD/$ARCHIVE"
  SUM_BEFORE=$(sha256sum "$ARCHIVE" | cut -d' ' -f1)
  # Corrupt the sqlite header of the restored copy.
  printf 'CORRUPTED!!!!!!!' | dd of="$CHOME/forge.sqlite3" bs=1 count=16 conv=notrunc 2>/dev/null
  run env FORGE_HOME="$CHOME" FORGE_HTTP=127.0.0.1:7342 timeout 20 $FORGE daemon start --foreground
  echo "(the daemon above must refuse to start on the corrupted db)" | tee -a "$LOG"
  SUM_AFTER=$(sha256sum "$ARCHIVE" | cut -d' ' -f1)
  [ "$SUM_BEFORE" = "$SUM_AFTER" ] && echo "backup archive intact: OK" | tee -a "$LOG" || echo "backup archive CHANGED" | tee -a "$LOG"
}

step3() {
  say "3. rollback dry run: healthy start records prev/, rollback restores the db (scratch home)"
  RBHOME=$(mktemp -d /tmp/forge-m12-rollback.XXXXXX)
  env FORGE_HOME="$RBHOME" FORGE_HTTP=127.0.0.1:7343 $FORGE daemon start --foreground \
    > "$SCRATCH/rollback-daemon.log" 2>&1 &
  DPID=$!
  sleep 3
  kill "$DPID" 2>/dev/null; wait "$DPID" 2>/dev/null
  run ls -l "$RBHOME/prev"
  echo "(prev/forge-bin-good and prev/db-good must exist after a healthy start)" | tee -a "$LOG"
  run env FORGE_HOME="$RBHOME" $FORGE daemon rollback
  run ls "$RBHOME"
  echo "(forge.sqlite3 restored; the broken one set aside as forge.sqlite3.broken-*)" | tee -a "$LOG"
}

step4() {
  say "4. forge eval scores two case sets differently (simulating a worse prompt version)"
  run $FORGE eval --mode run --cases evals --fixtures testdata/fixtures --prompt-version good
  # The "worse version": the same cases, but the inventory case expects
  # success while replaying the failing fixture — a version whose prompt
  # regressed would score exactly like this.
  WORSE=$(mktemp -d /tmp/forge-m12-worse.XXXXXX)
  cp -r evals/run "$WORSE/run"
  sed -i 's/^fixture = "inventory"/fixture = "failing"/' "$WORSE/run/inventory/eval.toml"
  run $FORGE eval --mode run --cases "$WORSE" --fixtures "$PWD/testdata/fixtures" --prompt-version worse
  echo "(the second score must be lower; its non-zero exit is the finding, not a smoke failure)" | tee -a "$LOG"
}

step5() {
  say "5. a routine proposal without an eval score cannot be approved (409)"
  API=http://127.0.0.1:7340/api/v1
  PID=$(curl -s -X POST "$API/proposals" -H 'Content-Type: application/json' \
    -d '{"kind":"routine","target":"routine:m12-smoke","after":{"prompt":"x"},"rationale":"m12 smoke","verification_plan":"forge eval"}' \
    | python3 -c 'import json,sys;print(json.load(sys.stdin)["id"])')
  echo "proposal: $PID" | tee -a "$LOG"
  CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API/proposals/$PID/approve")
  echo "approve without eval score -> HTTP $CODE (must be 409)" | tee -a "$LOG"
  run curl -s -X POST "$API/proposals/$PID/reject" -H 'Content-Type: application/json' -d '{"reason":"m12 smoke cleanup"}'
}

steps=("$@"); [ ${#steps[@]} -eq 0 ] && steps=(1 2 3 4 5)
for s in "${steps[@]}"; do "step$s"; done
echo; echo "log: $LOG"
