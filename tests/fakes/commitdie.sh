#!/bin/bash
# attempt 1: commits the right answer, then dies before reporting.
# attempt 2 (sees feedback): changes nothing and honestly reports no changes.
source "$(dirname "$0")/lib.sh"
prompt=$(cat)
if grep -q 'previous attempt ended without a result' <<<"$prompt"; then
  result "already committed last time"
else
  echo 42 > answer.txt && git add -A && git commit -qm "answer"
  exit 1
fi
