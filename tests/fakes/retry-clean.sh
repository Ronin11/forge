#!/bin/bash
set -e
source "$(dirname "$0")/lib.sh"
cat >/dev/null
test ! -e tests/acceptance
test -z "$(git status --porcelain)"
test -z "$(git ls-files tests/acceptance)"
grep -qx 42 answer.txt
echo extra > extra.txt
git add extra.txt
git commit -qm "complete retry from clean predecessor"
result "retry started clean" extra.txt:added
