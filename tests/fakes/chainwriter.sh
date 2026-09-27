#!/bin/bash
# the code step for planner-chain.sh's plan items: writes answer.txt=42
# for the first item, or the other named file for a later one, each a
# real, new commit.
source "$(dirname "$0")/lib.sh"
prompt=$(cat)
if grep -q 'Add answer.txt' <<<"$prompt"; then
  echo 42 > answer.txt
  git add -A && git commit -qm "answer" >/dev/null
  result "wrote the answer" answer.txt:added
else
  name=$(grep -oE '[a-zA-Z0-9_-]+\.txt' <<<"$prompt" | grep -v -e answer.txt -e hello.txt | head -1)
  echo ok > "$name"
  git add -A && git commit -qm "wrote $name" >/dev/null
  result "wrote $name" "$name:added"
fi
