#!/bin/bash
# the investigate step: reads, changes nothing, returns a plan naming real paths
source "$(dirname "$0")/lib.sh"
cat >/dev/null
result "Plan: add answer.txt at the repository root containing 42. Leave hello.sh as it is; it is unrelated. The proof is the repository check named answer, which reads answer.txt, plus the existing checks in forge.toml. One commit, one file."
