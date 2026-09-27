#!/bin/bash
# the investigate step: reads, changes nothing, returns a plan whose three
# items each add a distinct file, so a `direct` workflow chain of code
# steps (see chainwriter.sh) produces a real commit at every link instead
# of repeating the same, already-landed change.
source "$(dirname "$0")/lib.sh"
cat >/dev/null
result "Add answer.txt at the repository root containing 42, proven by the repository check named answer in forge.toml; leave hello.sh exactly as it is.\n\nAdd sibling-b.txt at the repository root, any content: a second link in the same initiative.\n\nAdd sibling-c.txt at the repository root, any content: closes the initiative's outcome."
