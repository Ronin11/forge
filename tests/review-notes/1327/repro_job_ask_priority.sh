#!/usr/bin/env bash
# Reproduction: the blocked "job question" task that job::failure::ask files
# (src/job/failure.rs, on a job's on_failure = "ask:*" or exhausted retries)
# never sets Task.priority, so it silently gets Rust's i64 default (0,
# lowest) instead of the documented default of 2 ("normal", see
# store::priority / store::PRIORITY_DEFAULT), unlike deploy::ask (src/deploy.rs)
# which sets `priority: crate::store::PRIORITY_DEFAULT` explicitly for the
# same shape of task.
#
# Run from a fresh clone's repository root:
#   cargo build --bin forge
#   bash tests/review-notes/1327/repro_job_ask_priority.sh
#
# Expect: `forge show` prints "priority   0" for the blocked job-question
# task, when it should either print nothing (priority 2, the default) or
# whatever the operator would consider "normal" for a job's own escalation.
set -euo pipefail

BIN="$(pwd)/target/debug/forge"
if [ ! -x "$BIN" ]; then
  echo "build target/debug/forge first (cargo build --bin forge)" >&2
  exit 1
fi

DIR="$(mktemp -d)"
trap 'rm -rf "$DIR"' EXIT
HOME_DIR="$DIR/home"
REPO="$DIR/repo"
ORIGIN="$DIR/origin.git"
XDG="$DIR/xdg_config"

mkdir -p "$REPO"
git -C "$REPO" init -q -b main
git -C "$REPO" config user.name Test
git -C "$REPO" config user.email test@example.com
cat > "$REPO/forge.toml" <<'EOF'
[checks]
answer = ["bash", "-c", "test -f answer.txt && grep -qx 42 answer.txt"]
shell = ["bash", "-n", "hello.sh"]
EOF
printf '#!/bin/bash\necho hello\n' > "$REPO/hello.sh"
git -C "$REPO" add -A
git -C "$REPO" commit -qm init
git init -q --bare "$ORIGIN"
git -C "$REPO" remote add origin "$ORIGIN"

export FORGE_HOME="$HOME_DIR"
export XDG_CONFIG_HOME="$XDG"
export FORGE_CLAUDE_BIN="$(pwd)/tests/fakes/ok.sh"
export FORGE_SANDBOX=0
export FORGE_SUPERVISOR=0

"$BIN" project new equitizr --purpose p --repo "$REPO" >/dev/null
"$BIN" workflows >/dev/null

mkdir -p "$HOME_DIR/workflows"
cat > "$HOME_DIR/workflows/always-fails.toml" <<'EOF'
name = "always-fails"
kind = "run"
description = "writes a file and always fails its assertion"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"

[assert]
clean = ["bash", "-c", "exit 1"]

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "ask:operator"
EOF

INPUT="$DIR/input.json"
echo '{"path":"out.txt","content":"hello"}' > "$INPUT"

JOB_ID=$("$BIN" job start equitizr always-fails --input "$INPUT" --now)
echo "job id: $JOB_ID"

"$BIN" job show "$JOB_ID" --json | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["state"]=="needs_human", d'

TASK_ID=$("$BIN" requests --json | python3 -c '
import json, sys
rows = json.load(sys.stdin)
assert len(rows) == 1, rows
print(rows[0]["id"])
')
echo "blocked task id: $TASK_ID"

echo "--- forge show $TASK_ID ---"
"$BIN" show "$TASK_ID"

PRIORITY=$("$BIN" show "$TASK_ID" --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["priority"])')
echo "priority recorded: $PRIORITY"

if [ "$PRIORITY" != "2" ]; then
  echo "DEFECT CONFIRMED: job::failure::ask filed a task with priority=$PRIORITY, not the documented default of 2" >&2
  exit 1
fi
echo "no defect: priority was 2 as expected"
