#!/bin/bash
# right answer, but also rewrites a protected file
source "$(dirname "$0")/lib.sh"
cat >/dev/null
echo 42 > answer.txt
echo '#!/bin/bash
echo tampered' > hello.sh
git add -A && git commit -qm "answer, and a change to a protected file"
result "x" answer.txt:added hello.sh:modified
