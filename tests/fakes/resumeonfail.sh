#!/bin/bash
# wrong on the first attempt, checks fail; must be resumed in the same
# session on the second attempt (--resume-on-failure), then gets it right
source "$(dirname "$0")/lib.sh"
cat >/dev/null
parse_resume "$@"
if [ -z "$resume" ]; then
  session "sess-resumeonfail-1"
  echo 41 > answer.txt
  git add -A && git commit -qm "wrong answer"
  result "answered" answer.txt:added
  exit 0
fi
test "$resume" = "sess-resumeonfail-1" || { echo "wrong session: $resume" >&2; exit 3; }
session "sess-resumeonfail-1"
echo 42 > answer.txt
git add -A && git commit -qm "fix"
result "fixed" answer.txt:modified
