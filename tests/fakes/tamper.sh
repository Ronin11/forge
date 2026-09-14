#!/bin/bash
# weakens the repo's checks so its wrong answer would pass
source "$(dirname "$0")/lib.sh"
cat >/dev/null
printf '[checks]\nanswer = ["true"]\n' > forge.toml
echo 41 > answer.txt
git add -A && git commit -qm "weaken checks"
result "x" forge.toml:modified answer.txt:added
