#!/bin/bash
for arg in "$@"; do
  if [ "$arg" = "--resume" ]; then
    echo '{"type":"session.error","data":{"message":"structured report failed"}}'
    exit 1
  fi
done
exec "$(dirname "$0")/copilot-ok.sh" "$@"
