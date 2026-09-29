#!/bin/bash
set -eu
source "$(dirname "$0")/lib.sh"

# The nudge must use Forge's registered identity, not the clone's poisoned one.
resumed=0
for arg in "$@"; do
  if [ "$arg" = "resume" ]; then resumed=1; fi
  if [ "$arg" = "--output-schema" ]; then
    test "$GIT_CONFIG_VALUE_0" = "Registered Author"
    codex_result "wrote 42" answer.txt:added
    exit 0
  fi
done

codex_thread "codex-nudge-metadata"
if [ "$resumed" = "1" ]; then
  test "$GIT_CONFIG_VALUE_0" = "Registered Author"
  # The fake agent itself must not execute the monitor either.
  git -c core.fsmonitor=false -c core.hooksPath=/dev/null add answer.txt
  git -c core.fsmonitor=false -c core.hooksPath=/dev/null commit -qm answer
else
  echo 42 > answer.txt
  git config user.name "Agent Poison"
  git config user.email "poison@example.com"
  marker="$(dirname "$PWD")/host-nudge-fsmonitor-marker"
  git config core.fsmonitor "echo fsmonitor > '$marker'; false"
fi
echo '{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"ready"}}'
codex_usage 100 10 50 5
