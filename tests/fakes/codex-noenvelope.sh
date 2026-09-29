#!/bin/bash
for arg in "$@"; do
  if [ "$arg" = "--output-schema" ]; then
    echo '{"type":"turn.failed","error":{"message":"structured report failed"}}'
    exit 1
  fi
done
exec "$(dirname "$0")/codex-ok.sh" "$@"
