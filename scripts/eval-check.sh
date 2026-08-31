#!/usr/bin/env bash
# M12 eval gate: run the golden cases under evals/ through the fake-claude
# executor. No network, no `claude`, no budget — every case spins an isolated
# temporary FORGE_HOME. Exits non-zero when any case fails.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ ! -x ./forge ]; then
  echo "eval-check: ./forge missing; run 'just build' first" >&2
  exit 1
fi
./forge eval --mode run --cases evals --fixtures testdata/fixtures
