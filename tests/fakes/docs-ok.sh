#!/bin/bash
# a docs task that stays in scope
source "$(dirname "$0")/lib.sh"
cat >/dev/null
echo "# notes" > NOTES.md; git add -A && git commit -qm "notes"
result "x" NOTES.md:added
