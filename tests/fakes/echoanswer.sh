#!/bin/bash
# writes the number the task names; told the base moved, merges it and keeps its own answer
source "$(dirname "$0")/lib.sh"
prompt=$(cat)
n=$(grep -oE 'write [0-9]+' <<<"$prompt" | head -1 | grep -oE '[0-9]+')
if grep -q 'conflicts in' <<<"$prompt"; then
  git merge forge/main >/dev/null 2>&1 || true
  echo "$n" > answer.txt; git add -A && git commit -qm "merge main, keep $n" >/dev/null
  result "merged main and kept my answer" answer.txt:modified
else
  # the task that writes 42 is the slow one, so the other lands first and 42 always conflicts
  [ "$n" = "42" ] && sleep 1
  echo "$n" > answer.txt; git add -A && git commit -qm "answer $n" >/dev/null
  result "answer" answer.txt:added
fi
