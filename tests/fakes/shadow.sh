#!/bin/bash
# a coder that plants a file inside the verification namespace
source "$(dirname "$0")/lib.sh"
cat >/dev/null
echo 42 > answer.txt; mkdir -p tests/acceptance; printf '#!/bin/bash\ntrue\n' > tests/acceptance/answer.sh
git add -A && git commit -qm "answer, and a shadow test"
result "x" answer.txt:added tests/acceptance/answer.sh:added
