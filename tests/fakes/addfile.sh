#!/bin/bash
# adds extra.txt, whatever else the tree holds; pauses first when FAKE_SLEEP
# is set, so another task can land meanwhile
source "$(dirname "$0")/lib.sh"
cat >/dev/null
[ -n "$FAKE_SLEEP" ] && sleep 3
echo extra > extra.txt; git add -A && git commit -qm "extra"
result "added extra" extra.txt:added
