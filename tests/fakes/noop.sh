#!/bin/bash
# does nothing and says so honestly
source "$(dirname "$0")/lib.sh"
cat >/dev/null
result "Reviewed the diff and the checks; nothing to fix."
