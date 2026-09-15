#!/bin/bash
# the investigate step: reads, changes nothing, returns a plan with three
# blank-line separated items, for testing `initiative from-plan` and the
# `file_into_initiative` flag (both split a plan into paragraphs)
source "$(dirname "$0")/lib.sh"
cat >/dev/null
result "Add answer.txt at the repository root containing 42, proven by the repository check named answer in forge.toml.\n\nLeave hello.sh exactly as it is; it already satisfies the shell check and this step changes nothing there.\n\nRecord that the initiative's outcome is met once the check named answer passes, which closes the loop this plan describes."
