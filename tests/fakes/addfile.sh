#!/bin/bash
# adds extra.txt, whatever else the tree holds; pauses first when FAKE_SLEEP
# is set, so another task can land meanwhile
source "$(dirname "$0")/lib.sh"
cat >/dev/null
if [ -n "$FAKE_GATE" ]; then
  touch .git/gate-ready
  deadline=$((SECONDS + 600))
  until [ -e .git/gate-open ]; do
    [ "$SECONDS" -lt "$deadline" ] || exit 1
    sleep 0.1
  done
elif [ -n "$FAKE_SLEEP" ]; then
  sleep 3
fi
echo extra > extra.txt; git add -A && git commit -qm "extra"
result "added extra" extra.txt:added
