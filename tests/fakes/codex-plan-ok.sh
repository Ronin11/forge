#!/bin/bash
# A fake codex CLI for the investigate step (contract "plan"): reads,
# changes nothing, and returns a plan naming real paths, the same shape
# codex-ok.sh gives the code step but with no command execution or
# changes, since this contract never touches the tree.
source "$(dirname "$0")/lib.sh"

argv_debug "${@:1:$#-1}"

codex_thread "codex-fake-plan-1"
codex_result "Plan: add answer.txt at the repository root containing 42. The proof is the repository check named answer, which reads answer.txt."
codex_usage 100 10 50 5
