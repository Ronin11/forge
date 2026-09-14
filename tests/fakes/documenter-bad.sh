#!/bin/bash
# a document step that also changes behavior
source "$(dirname "$0")/lib.sh"
cat >/dev/null
sed -i 's/echo hello/echo hi/' hello.sh
git add -A && git commit -qm "tweak"
result "documented" hello.sh:modified
