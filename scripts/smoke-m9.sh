#!/usr/bin/env bash
# M9 smoke: parallelism + integration (SMOKE.md §M9, steps 1-8).
# Runs against the LIVE ~/.forge daemon. All git surfaces are scratch: a bare
# remote under ~/.forge/.scratch/remotes and a scratch clone of the equitizr
# CHECKOUT under ~/.forge/.scratch/m9repo — ~/Projects/equitizr itself is
# never modified and its real origin is never pushed to.
set -u
cd "$(dirname "$0")/.."
FORGE=./forge
SCRATCH="$HOME/.forge/.scratch"
REMOTE="$SCRATCH/remotes/eq.git"
REPO="$SCRATCH/m9repo"
REPO2="$SCRATCH/m9repo2"
REMOTE2="$SCRATCH/remotes/eq2.git"
LOG=.scratch/smoke-m9.log
mkdir -p .scratch
: > "$LOG"
say() { echo; echo "### $*" | tee -a "$LOG"; }
run() { echo "\$ $*" | tee -a "$LOG"; "$@" 2>&1 | tee -a "$LOG"; }

# task_state ID -> derived work state
task_state() { $FORGE task show "$1" --json 2>/dev/null | python3 -c 'import json,sys;print(json.load(sys.stdin)["state"])' 2>/dev/null; }
target_state() { $FORGE task show "$1" --json 2>/dev/null | python3 -c 'import json,sys;print(json.load(sys.stdin)["targets"][0]["state"])' 2>/dev/null; }
wait_state() { # ID want... : poll up to 10 min
  local id=$1; shift
  for i in $(seq 1 120); do
    st=$(task_state "$id")
    for want in "$@"; do [ "$st" = "$want" ] && { echo "$id -> $st" | tee -a "$LOG"; return 0; }; done
    sleep 5
  done
  echo "TIMEOUT: $id stuck at $st" | tee -a "$LOG"; return 1
}
# add_task <prompt> [task add flags...] -> prints work id
add_task() {
  local prompt=$1; shift
  $FORGE task add "$prompt" --repo "$REPO" --model haiku --autonomy auto --force "$@" --json \
    | python3 -c 'import json,sys;print(json.load(sys.stdin)["work"]["id"])'
}

setup() {
  say "setup: bare remote + scratch clone of the equitizr checkout, registered on the fly"
  rm -rf "$REMOTE" "$REPO"
  mkdir -p "$SCRATCH/remotes"
  git init --bare -b master "$REMOTE"
  # Cloning the checkout only READS it (constitution 1); the scratch clone's
  # origin is re-pointed at the bare remote so no real remote is ever pushed.
  git clone "$HOME/Projects/equitizr" "$REPO"
  git -C "$REPO" remote set-url origin "$REMOTE"
  BR=$(git -C "$REPO" symbolic-ref --short HEAD)
  if [ "$BR" != master ]; then git -C "$REPO" branch -m "$BR" master; fi
  cat > "$REPO/forge.toml" <<'EOF'
integration_branch = "master"
task_branches = "forge/*"

[checks]
smoke = ["true"]
EOF
  git -C "$REPO" add forge.toml
  git -C "$REPO" -c user.name=smoke -c user.email=smoke@localhost commit -m "forge.toml: integration branch for M9 smoke"
  git -C "$REPO" push -u origin master
  run git -C "$REMOTE" rev-parse master
  # Register with the live daemon by path (repositories on the fly).
  run $FORGE task add "Read-only: reply with one short sentence and an empty changes list." --repo "$REPO" --model haiku --autonomy auto --wait
}

step1() {
  say "1. disjoint paths run concurrently; overlapping paths serialize with path_lease"
  A=$(add_task "Create the file zoneA/a.txt containing the single line 'alpha' (mkdir -p zoneA). Commit it with message 'zoneA'. Then WAIT: run exactly 'sleep 60' once before writing your final summary. Declare exactly zoneA/a.txt in changes." --paths 'zoneA/**')
  B=$(add_task "Create the file zoneB/b.txt containing the single line 'beta' (mkdir -p zoneB). Commit it with message 'zoneB'. Declare exactly zoneB/b.txt in changes." --paths 'zoneB/**')
  C=$(add_task "Append the line 'gamma' to zoneA/a.txt if it exists, else create it (mkdir -p zoneA). Commit. Declare exactly zoneA/a.txt in changes." --paths 'zoneA/**')
  sleep 25
  run $FORGE queue
  echo "expect: C pending with path_lease (overlaps A); B running/succeeded alongside A" | tee -a "$LOG"
  $FORGE queue --json | python3 -c '
import json,sys
rows=json.load(sys.stdin)
for r in rows:
    print(r["work"]["id"][:8], r["state"], r.get("reason",""))' | tee -a "$LOG"
  wait_state "$A" succeeded merging merged partial failed unverified
  wait_state "$B" succeeded merging merged partial failed unverified
  wait_state "$C" succeeded merging merged partial failed unverified
}

step2() {
  say "2. deps = [...] with integrate: refused loudly (documented M9 gap: no deps pre-step)"
  # The CLI has no --deps flag; the routine API carries deps — drive it there.
  python3 - <<'EOF' 2>&1 | tee -a "$LOG"
import json,urllib.request
body={"name":"m9deps2","mode":"run","prompt":"never runs","repositories":["m9repo"],"model":"haiku","timeout_seconds":300,"deps":["left-pad"],"integrate":True}
req=urllib.request.Request("http://127.0.0.1:7340/api/v1/routines",json.dumps(body).encode(),{"Content-Type":"application/json"})
try:
    print(urllib.request.urlopen(req).status)
except Exception as e:
    print("create:",e)
req=urllib.request.Request("http://127.0.0.1:7340/api/v1/routines/m9deps2/run",b"{}",{"Content-Type":"application/json"})
try:
    print("run:",urllib.request.urlopen(req).status,"(BUG: should have been refused)")
except urllib.error.HTTPError as e:
    print("run refused:",e.status,e.read().decode())
EOF
}

step3() {
  say "3. plan on a small goal: >=3 tasks with paths and edges appear as a batch"
  P=$($FORGE task add "Goal: add three tiny marker files to this repository — docs-marker/M.md describing the repo in one line, scripts-marker/run.sh that echoes ok, and a top-level MARKERS.md that lists both files (this one depends on the first two). Split this into exactly three tasks with honest per-task paths globs; the third task is blocked_by the first two." \
      --repo "$REPO" --mode plan --model sonnet --autonomy auto --force --integrate --json \
      | python3 -c 'import json,sys;print(json.load(sys.stdin)["work"]["id"])')
  wait_state "$P" succeeded partial failed unverified
  run $FORGE queue
  $FORGE task list --json | python3 -c '
import json,sys
rows=json.load(sys.stdin)
batch=[r for r in rows if r["work"].get("plan_batch_id")=="'"$P"'"]
print("batch size:",len(batch))
for r in batch: print(" ", r["work"]["id"][:8], r["state"], r["work"].get("paths"), "integrate:", r["work"]["integrate"])
assert len(batch)>=3, "plan batch missing"' | tee -a "$LOG"
}

step4() {
  say "4. merge queue: three integrating tasks land on the remote fast-forward-only, journaled SHAs"
  BEFORE=$(git -C "$REMOTE" rev-parse master)
  M1=$(add_task "Create mq/one.txt with the line 'one' (mkdir -p mq); commit with message 'mq one'. Declare exactly mq/one.txt in changes." --paths 'mq/one*' --integrate)
  M2=$(add_task "Create mq/two.txt with the line 'two' (mkdir -p mq); commit with message 'mq two'. Declare exactly mq/two.txt in changes." --paths 'mq/two*' --integrate)
  M3=$(add_task "Create mq/three.txt with the line 'three' (mkdir -p mq); commit with message 'mq three'. Declare exactly mq/three.txt in changes." --paths 'mq/three*' --integrate)
  for id in "$M1" "$M2" "$M3"; do wait_state "$id" merged partial failed unverified conflict; done
  AFTER=$(git -C "$REMOTE" rev-parse master)
  echo "remote master: $BEFORE -> $AFTER" | tee -a "$LOG"
  git -C "$REMOTE" merge-base --is-ancestor "$BEFORE" "$AFTER" && echo "fast-forward only: OK" | tee -a "$LOG"
  run git -C "$REMOTE" log --oneline -5 master
  curl -s "http://127.0.0.1:7340/api/v1/journal?since=0&limit=1000" | python3 -c '
import json,sys
rows=json.load(sys.stdin)
pushed=[r for r in rows if r.get("kind")=="merge.pushed"]
print("merge.pushed rows:",len(pushed))
for r in pushed[-3:]: print(" ", r["payload"].get("before","")[:8],"->",r["payload"].get("after","")[:8])' | tee -a "$LOG"
}

step5() {
  say "5. engineered conflict (declared paths lie): integrate resolves or conflict retained; precision < 1"
  X=$(add_task "Overwrite the entire file conflict.txt so its only content is the line 'version X'. Create it if absent. Commit with message 'X'. Declare exactly conflict.txt in changes." --paths 'zoneX/**' --integrate)
  Y=$(add_task "Overwrite the entire file conflict.txt so its only content is the line 'version Y'. Create it if absent. Commit with message 'Y'. Declare exactly conflict.txt in changes." --paths 'zoneY/**' --integrate)
  wait_state "$X" merged conflict partial failed unverified
  wait_state "$Y" merged conflict partial failed unverified
  run $FORGE task show "$X"
  run $FORGE task show "$Y"
  for id in "$X" "$Y"; do
    aid=$($FORGE task show "$id" --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["attempts"][0]["id"])')
    curl -s "http://127.0.0.1:7340/api/v1/attempts/$aid" | python3 -c '
import json,sys
f=json.load(sys.stdin).get("facts") or {}
print("precision:",f.get("write_set_precision"),"declared:",f.get("declared_paths"),"touched:",f.get("touched_paths"),"merge:",f.get("merge_outcome"))' | tee -a "$LOG"
  done
  ls "$HOME/.forge/integrator/m9repo/" 2>/dev/null | tee -a "$LOG" || true
  echo "expect: one merged, one in conflict with retained scratch (integrate-mode auto-spawn is a documented M9 gap)" | tee -a "$LOG"
}

step6() {
  say "6. stacking: B starts on A's branch head before A merges; both merge"
  A=$(add_task "Create stack/base.txt with the single line 'layer A' (mkdir -p stack); commit with message 'stack A'. Declare exactly stack/base.txt in changes." --paths 'stack/**' --integrate)
  # Wait until A has succeeded (agent work done, awaiting merge), then stack B on it.
  for i in $(seq 1 120); do
    st=$(target_state "$A"); case "$st" in succeeded|queued_for_merge|merging|merged) break;; esac; sleep 5
  done
  # B is created already blocked on A (--after) so it cannot be claimed in
  # the gap before the edge is upgraded to stack_on (INSERT OR REPLACE).
  B=$($FORGE task add "Read stack/base.txt (it exists on your branch) and create stack/on-top.txt containing its content plus the line 'layer B'; commit with message 'stack B'. Declare exactly stack/on-top.txt in changes." \
      --repo "$REPO" --model haiku --autonomy auto --force --paths 'stack/**' --integrate --after "$A" --json \
      | python3 -c 'import json,sys;print(json.load(sys.stdin)["work"]["id"])')
  run $FORGE queue block "$B" --on "$A" --stack
  echo "note: if the integrator merged A before B claimed, B runs on the new master instead (stack_base_commit empty) — both orders are correct; the stacked order is the interesting one" | tee -a "$LOG"
  wait_state "$A" merged partial failed unverified conflict
  wait_state "$B" merged partial failed unverified conflict
  aid=$($FORGE task show "$B" --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["attempts"][0]["id"])')
  curl -s "http://127.0.0.1:7340/api/v1/attempts/$aid" | python3 -c '
import json,sys
d=json.load(sys.stdin)
print("B stack_base_commit:",d["attempt"].get("stack_base_commit"),"| facts stack_depth:",(d.get("facts") or {}).get("stack_depth"))' | tee -a "$LOG"
  run git -C "$REMOTE" show master:stack/on-top.txt
}

step7() {
  say "7. mergiraf resolves an adjacent-addition Go conflict with no agent involved"
  git -C "$REPO" pull --ff-only origin master 2>&1 | tee -a "$LOG"
  cat > "$REPO/m9demo.go" <<'EOF'
package m9demo

func a() int { return 1 }

func z() int { return 26 }
EOF
  git -C "$REPO" add m9demo.go
  git -C "$REPO" -c user.name=smoke -c user.email=smoke@localhost commit -m "m9demo base"
  git -C "$REPO" push origin master
  P=$(add_task "In m9demo.go, insert exactly 'func b() int { return 2 }' (with a blank line around it) between func a and func z. Commit with message 'add b'. Declare exactly m9demo.go in changes." --paths 'm9demo.go' --integrate)
  Q=$(add_task "In m9demo.go, insert exactly 'func c() int { return 3 }' (with a blank line around it) between func a and func z. Commit with message 'add c'. Declare exactly m9demo.go in changes." --paths 'm9demoX/**' --integrate)
  wait_state "$P" merged partial failed unverified conflict
  wait_state "$Q" merged partial failed unverified conflict
  run git -C "$REMOTE" show master:m9demo.go
  echo "expect: both merged; the file carries func b AND func c (mergiraf solve, journal: merge.pushed x2)" | tee -a "$LOG"
}

step8() {
  say "8. push policy: no integration_branch -> refused + journaled; --force impossible by construction"
  rm -rf "$REMOTE2" "$REPO2"
  git init --bare -b master "$REMOTE2"
  git clone "$REPO" "$REPO2"
  git -C "$REPO2" remote set-url origin "$REMOTE2"
  git -C "$REPO2" rm -q forge.toml
  git -C "$REPO2" -c user.name=smoke -c user.email=smoke@localhost commit -qm "no forge.toml: pushes must be refused"
  git -C "$REPO2" push -q -u origin master
  R=$($FORGE task add "Create refused.txt containing 'x'; commit with message 'refused'. Declare exactly refused.txt in changes." \
      --repo "$REPO2" --model haiku --autonomy auto --force --paths 'refused*' --integrate --json \
      | python3 -c 'import json,sys;print(json.load(sys.stdin)["work"]["id"])')
  wait_state "$R" conflict partial failed unverified merged
  tid=$($FORGE task show "$R" --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["targets"][0]["id"])')
  curl -s "http://127.0.0.1:7340/api/v1/journal?since=0&limit=1000" | python3 -c '
import json,sys
rows=json.load(sys.stdin)
ref=[r for r in rows if r.get("kind")=="merge.refused" and r.get("entity_id")=="'"$tid"'"]
print("merge.refused rows for target:",len(ref))
for r in ref: print(" ", r["payload"].get("reason"))' | tee -a "$LOG"
  echo "remote2 heads (must be only the setup commit):" | tee -a "$LOG"
  run git -C "$REMOTE2" log --oneline -3 master
  say "8b. no --force/-f/+refspec in any push invocation (source grep + unit test)"
  run grep -rn -- '"push"' internal/integrator/push.go
  ! grep -rn -- '--force\|"+refs/' internal/integrator/*.go | grep -v _test.go | tee -a "$LOG"
  run go test -count=1 -run TestNoForceInPushSource ./internal/integrator
}

steps=("$@"); [ ${#steps[@]} -eq 0 ] && steps=(1 2 3 4 5 6 7 8)
setup
for s in "${steps[@]}"; do "step$s"; done
echo; echo "log: $LOG"
