#!/bin/bash
# The interview directive the moment a person says they want to stop: no
# more questions, needs_input null, a short plain-sentence summary. Not a
# repository plan, so it must not be held to plan-substantive.
source "$(dirname "$0")/lib.sh"
cat >/dev/null
result "They asked to stop."
