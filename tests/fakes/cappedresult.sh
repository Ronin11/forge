#!/bin/bash
# reports hitting the turn cap but still returns a proper result: a wrong answer the first time, right the second
source "$(dirname "$0")/lib.sh"
cat >/dev/null
if [ -f answer.txt ] && grep -qx 41 answer.txt; then
  echo 42 > answer.txt; git add -A && git commit -qm "fix"
  result "fixed" answer.txt:modified
  exit 0
fi
parse_resume "$@"
[ -n "$resume" ] && { echo "must not be resumed" >&2; exit 3; }
session "sess-capped-1"
echo 41 > answer.txt; git add -A && git commit -qm "answer"
result "answered" answer.txt:added
