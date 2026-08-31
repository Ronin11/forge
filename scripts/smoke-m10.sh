#!/usr/bin/env bash
# M10 smoke: runners, models, and routing (SMOKE spec §M10, steps 1-6).
# Nothing here spends budget: every attempt runs the fake-claude executor over
# checked-in fixtures (testdata/fixtures/*). To keep the LIVE ~/.forge daemon
# and config untouched, everything runs against a TEMPORARY forge home with its
# own daemon+worker on a scratch port.
#
# Honesty notes:
#   - The dev box's real OpenAI-compatible endpoint is not assumed reachable.
#     Step 1 configures runner `devbox` pointing at it; if it is down the worker
#     advertises `runner:devbox down|unauthenticated`, which is itself the test.
#     The executor stays fake-claude (the `pi` executor is not shipped in this
#     tree), so a kimi claim "runs" through fake-claude over a fixture.
#   - fake-claude's fixture is chosen by the worker's FORGE_FAKE_FIXTURE env, so
#     switching between a passing and a failing run means restarting the worker
#     with a different fixture directory (fixture_ok / fixture_fail below).
#   - Steps 2, 4, 6 are fully fixture-driven and deterministic.
# Run from the repo root: scripts/smoke-m10.sh [step...]
set -u
cd "$(dirname "$0")/.."
FORGE=./forge
LOG=.scratch/smoke-m10.log
SCRATCH=.scratch/m10
mkdir -p .scratch "$SCRATCH"
: > "$LOG"
say() { echo; echo "### $*" | tee -a "$LOG"; }
run() { echo "\$ $*" | tee -a "$LOG"; "$@" 2>&1 | tee -a "$LOG"; }

HOME_DIR=""
PORT=7350
DPID=""
WPID=""
FIX_OK="$PWD/testdata/fixtures/inventory" # passing stream
FIX_FAIL="$PWD/testdata/fixtures/failing" # verification-failing stream

fh() { env FORGE_HOME="$HOME_DIR" "$@"; }

# start_worker (re)starts the fake-claude worker bound to a fixture directory.
start_worker() {
  [ -n "$WPID" ] && { kill "$WPID" 2>/dev/null; wait "$WPID" 2>/dev/null; }
  env FORGE_HOME="$HOME_DIR" FORGE_FAKE_FIXTURE="$1" $FORGE worker start \
    > "$SCRATCH/worker.log" 2>&1 &
  WPID=$!
  sleep 2
}

setup() {
  say "setup: temp home with a devbox runner + kimi model; fake-claude worker"
  HOME_DIR=$(mktemp -d /tmp/forge-m10.XXXXXX)
  env FORGE_HOME="$HOME_DIR" FORGE_HTTP=127.0.0.1:$PORT $FORGE bootstrap >/dev/null 2>&1 || true
  # config.toml carries only the devbox override; the claude runner and the
  # haiku/sonnet/opus models come from the embedded defaults.
  cat >> "$HOME_DIR/config.toml" <<EOF

[runners.devbox]
kind = "openai-compatible"
billing = "api"
capacity = 1
endpoint = "http://127.0.0.1:11434/v1"

[models.kimi]
runner = "devbox"
id = "kimi-k2"
class = "mid"
max_tier = 3
price = { input = 0.20, output = 0.60 }
EOF
  # The worker probes runner:devbox against the same endpoint (down is valid).
  cat >> "$HOME_DIR/worker.toml" <<EOF

[runners.devbox]
kind = "openai-compatible"
endpoint = "http://127.0.0.1:11434/v1"
EOF
  # Use the fake-claude executor by default for every routine below.
  env FORGE_HOME="$HOME_DIR" FORGE_HTTP=127.0.0.1:$PORT $FORGE daemon start --foreground \
    > "$SCRATCH/daemon.log" 2>&1 &
  DPID=$!
  sleep 3
  start_worker "$FIX_OK"
}

teardown() {
  [ -n "$WPID" ] && kill "$WPID" 2>/dev/null
  [ -n "$DPID" ] && kill "$DPID" 2>/dev/null
  wait 2>/dev/null
}

# add_routine writes a routine TOML (any field) and applies it with --from.
add_routine() {
  local file="$SCRATCH/$1.toml"
  cat > "$file"
  run fh $FORGE routine add "$1" --from "$file"
}

step1() {
  say "1. configure devbox/kimi (executor fake-claude; pi not shipped); doctor shows runner health"
  run fh $FORGE doctor
  echo "(doctor lists runner:devbox ready|down|unauthenticated per the endpoint, and runs" | tee -a "$LOG"
  echo " the pricing-drift check once ≥3 priced attempts exist — see step 5)" | tee -a "$LOG"
  run fh $FORGE worker status
}

step2() {
  say "2. tier-0 routine models=[kimi,haiku] routes kimi-first; a failed verify escalates to haiku"
  add_routine router <<EOF
name = "router"
mode = "run"
prompt = "do the task in {{repo}}"
executor = "fake-claude"
model = "kimi"
tier = 0
models = ["kimi", "haiku"]
autonomy = "auto"
repositories = ["demo"]
timeout_seconds = 300
EOF
  start_worker "$FIX_FAIL"   # first attempt fails verification
  run fh $FORGE routine run router --wait
  TID=$(fh $FORGE task list --json | python3 -c 'import json,sys;print(json.load(sys.stdin)[0]["id"])')
  run fh $FORGE task show "$TID" --json
  echo "expect: attempt 1 model_alias=kimi with a routing decision recorded; verify fails." | tee -a "$LOG"
  run fh $FORGE task retry "$TID"
  run fh $FORGE routine run router --wait   # (worker claims the retried target)
  run fh $FORGE task show "$TID" --json
  echo "expect: the escalated attempt is model_alias=haiku, escalated_from=kimi, with the" | tee -a "$LOG"
  echo "        prior failure injected into its prompt." | tee -a "$LOG"
}

step3() {
  say "3. runner capacity 1: two kimi-only targets serialize on the devbox lease"
  add_routine kimionly <<EOF
name = "kimionly"
mode = "run"
prompt = "do {{repo}}"
executor = "fake-claude"
model = "kimi"
tier = 0
models = ["kimi"]
autonomy = "auto"
concurrency = 5
repositories = ["demo"]
timeout_seconds = 300
EOF
  start_worker "$FIX_OK"
  run fh $FORGE routine run kimionly
  run fh $FORGE routine run kimionly
  echo "expect: with devbox capacity 1, at most one kimi attempt runs at a time even" | tee -a "$LOG"
  echo "        though worker slots are free; the second waits on the runner lease." | tee -a "$LOG"
  sleep 3
  run fh $FORGE queue list
}

step4() {
  say "4. ≥10 runs at ~40% kimi verified-success → router stops choosing kimi (except explore)"
  add_routine mix <<EOF
name = "mix"
mode = "run"
prompt = "do {{repo}}"
executor = "fake-claude"
model = "kimi"
tier = 0
models = ["kimi", "haiku"]
autonomy = "auto"
repositories = ["demo"]
timeout_seconds = 300
EOF
  # 4 of 10 kimi runs pass, 6 fail — a scripted 40% verified-success. The
  # worker fixture selects pass/fail; restart it to switch.
  for i in $(seq 1 10); do
    if [ $((i % 5)) -le 1 ]; then start_worker "$FIX_OK"; else start_worker "$FIX_FAIL"; fi
    fh $FORGE routine run mix --wait >/dev/null 2>&1 || true
  done
  run fh $FORGE stats --json
  echo "expect: the capability matrix shows kimi verified-success ≈40% (below the gate);" | tee -a "$LOG"
  echo "        new mix targets route to haiku except during exploration." | tee -a "$LOG"
}

step5() {
  say "5. doctor flags pricing drift when the price table is edited wrong"
  sed -i 's/price = { input = 0.20, output = 0.60 }/price = { input = 20.0, output = 60.0 }/' "$HOME_DIR/config.toml"
  run fh $FORGE daemon restart
  sleep 3
  run fh $FORGE doctor
  echo "expect: the 'pricing' check warns that notional cost drifts from the reported" | tee -a "$LOG"
  echo "        total_cost_usd — the price table looks stale." | tee -a "$LOG"
}

step6() {
  say "6. a model-class overlay changes the composed prompt + its hash; stats split by prompt version"
  mkdir -p "$HOME_DIR/modes"
  cat > "$HOME_DIR/modes/run.small.md" <<'EOF'
SMALL-MODEL OVERLAY: keep the change minimal and explain each step.
EOF
  # haiku is class `small`, so the run.small.md overlay applies to it.
  add_routine overlay <<EOF
name = "overlay"
mode = "run"
prompt = "do {{repo}}"
executor = "fake-claude"
model = "haiku"
tier = 0
models = ["haiku"]
autonomy = "auto"
repositories = ["demo"]
timeout_seconds = 300
EOF
  start_worker "$FIX_OK"
  run fh $FORGE routine run overlay --wait
  run fh $FORGE stats --json
  echo "expect: the haiku (small) attempt's prompt_version_hash reflects the overlay;" | tee -a "$LOG"
  echo "        removing the overlay file and rerunning yields a different hash — stats" | tee -a "$LOG"
  echo "        split by prompt version show the two." | tee -a "$LOG"
}

steps=("$@")
[ ${#steps[@]} -eq 0 ] && steps=(1 2 3 4 5 6)
setup
trap teardown EXIT
for s in "${steps[@]}"; do "step$s"; done
echo | tee -a "$LOG"
echo "log: $LOG" | tee -a "$LOG"
