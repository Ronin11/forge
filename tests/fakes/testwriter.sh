#!/bin/bash
# the tests step: writes one acceptance test under the namespace that fails on base
source "$(dirname "$0")/lib.sh"
cat >/dev/null
mkdir -p tests/acceptance
printf '#!/bin/bash\ngrep -qx 42 answer.txt\n' > tests/acceptance/answer.sh
git add -A && git commit -qm "acceptance test for the answer"
result "Interface: a file answer.txt at the repo root whose entire content is the line 42 (no other lines). Trailing whitespace is not tolerated." tests/acceptance/answer.sh:added
