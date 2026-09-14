#!/bin/bash
# the tests step: its first test carries a TODO the repo's lint rejects; told so, it writes a clean one
source "$(dirname "$0")/lib.sh"
prompt=$(cat)
mkdir -p tests/acceptance
if grep -q 'every error is inside your tests' <<<"$prompt"; then
  printf '#!/bin/bash\ngrep -qx 42 answer.txt\n' > tests/acceptance/answer.sh
else
  printf '#!/bin/bash\n# TODO tighten\ngrep -qx 42 answer.txt\n' > tests/acceptance/answer.sh
fi
git add -A && git commit -qm "acceptance test for the answer"
result "Interface: a file answer.txt at the repo root whose entire content is the line 42 (no other lines). Trailing whitespace is not tolerated." tests/acceptance/answer.sh:added
