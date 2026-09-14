#!/bin/bash
# an investigator whose plan names files that do not exist
source "$(dirname "$0")/lib.sh"
cat >/dev/null
result "Plan: edit src/answer/mod.rs to return 42 and wire it through lib/main.rs; prove it with tests/answer_test.rs and the repository check named answer in forge.toml."
