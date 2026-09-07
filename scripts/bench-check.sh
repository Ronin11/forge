#!/usr/bin/env bash
# Runs every benchmark named in bench/threshold.txt and fails if any exceeds its
# ceiling. Output lines are "name ns/op ceiling status".
set -euo pipefail
cd "$(dirname "$0")/.."

# The ceilings in bench/threshold.txt are calibrated on the reference laptop, so
# they cannot be read literally on slower hardware. BENCH_SCALE multiplies every
# ceiling; CI sets it because a GitHub-hosted runner writes SQLite far slower
# than the laptop (BenchmarkInsertEvents: 2.08 ms/op on a runner vs 0.33 ms/op
# on the laptop, measured 2026-09-07). The laptop keeps the tight gate; CI keeps
# a loose one that still catches a catastrophic regression.
scale="${BENCH_SCALE:-1}"
status=0
while read -r name ceiling; do
    [[ -z "$name" || "$name" == \#* ]] && continue
    ceiling=$(( ceiling * scale ))
    line=$(go test -run '^$' -bench "^${name}\$" -benchtime 1s ./... 2>/dev/null | grep -E "^${name}(-[0-9]+)?\s" || true)
    if [[ -z "$line" ]]; then
        echo "bench: $name not found"; status=1; continue
    fi
    nsop=$(echo "$line" | awk '{print $3}')
    if (( ${nsop%.*} > ceiling )); then
        echo "bench: $name ${nsop} ns/op exceeds ${ceiling}"; status=1
    else
        echo "bench: $name ${nsop} ns/op ≤ ${ceiling} ok"
    fi
done < bench/threshold.txt
exit $status
