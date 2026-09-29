#!/bin/bash
# ok.sh, once the test opens the gate: holds its attempt until
# .git/gate-open appears in the worktree (or ten minutes pass), so a test
# acts while it runs however loaded the machine is
cat >/dev/null
for _ in $(seq 6000); do
  [ -e .git/gate-open ] && break
  sleep 0.1
done
exec "$(dirname "$0")/ok.sh" </dev/null
