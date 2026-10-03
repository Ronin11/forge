#!/bin/bash
# Hold the coder at an observable gate until the competing task has landed.
cat >/dev/null
touch .git/gate-ready
deadline=$((SECONDS + 600))
until [ -e .git/gate-open ]; do
  [ "$SECONDS" -lt "$deadline" ] || exit 1
  sleep 0.1
done
exec "$(dirname "$0")/addfile.sh" </dev/null
