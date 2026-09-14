#!/bin/bash
# writes the answer; when told a verification failed, also writes what it
# asked for. Pauses before the first answer when FAKE_SLEEP is set, so main
# can move underneath it.
source "$(dirname "$0")/lib.sh"
prompt=$(cat)
if grep -qE 'verification fail(ed|s)' <<<"$prompt"; then
  echo extra > extra.txt; git add -A && git commit -qm "extra"
  result "added extra" extra.txt:added
else
  [ -n "$FAKE_SLEEP" ] && sleep 2
  echo 42 > answer.txt; git add -A && git commit -qm "answer"
  result "answer" answer.txt:added
fi
