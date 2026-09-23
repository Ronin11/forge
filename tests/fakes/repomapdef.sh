#!/bin/bash
# Answers right only when forge-repomap is on PATH inside the sandbox and
# `forge-repomap def` prints the item it is asked for.
source "$(dirname "$0")/lib.sh"
cat >/dev/null
printf 'pub fn greet_the_world() {\n}\n' > greet.rs
git add -A
if forge-repomap def greet_the_world --dir . | grep -q greet_the_world; then
  echo 42 > answer.txt
else
  echo 41 > answer.txt
fi
git add -A && git commit -qm "answer"
result "x" answer.txt:added
