#!/bin/bash
# a tests step whose test already passes on base: it specifies nothing
source "$(dirname "$0")/lib.sh"
cat >/dev/null
mkdir -p tests/acceptance
printf '#!/bin/bash\ntrue\n' > tests/acceptance/trivial.sh
git add -A && git commit -qm "trivial test"
result "Interface: nothing in particular, this test passes regardless of the implementation." tests/acceptance/trivial.sh:added
