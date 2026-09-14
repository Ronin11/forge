#!/bin/bash
# a docs task that also touches code
source "$(dirname "$0")/lib.sh"
cat >/dev/null
echo "# notes" > NOTES.md; echo 42 > answer.txt; git add -A && git commit -qm "notes and answer"
result "x" NOTES.md:added answer.txt:added
