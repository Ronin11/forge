#!/bin/bash
# writes the answer; when told the merged base does not build, removes
# whatever broke it and commits the fix instead.
source "$(dirname "$0")/lib.sh"
prompt=$(cat)
if grep -q "does not build" <<<"$prompt"; then
  rm -f broken.txt
  git add -A && git commit -qm "remove what broke the merged base"
  result "removed broken.txt" broken.txt:deleted
else
  echo 42 > answer.txt && git add -A && git commit -qm "answer"
  result "answer" answer.txt:added
fi
