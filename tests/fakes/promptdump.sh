#!/bin/bash
# writes the right answer; the prompt it saw is in the log's first line anyway
source "$(dirname "$0")/lib.sh"
cat >/dev/null
echo 42 > answer.txt && git add -A && git commit -qm "answer"
result "x" answer.txt:added
