#!/usr/bin/env bash
# Runs every benchmark named in bench/threshold.txt and fails if any exceeds its
# ceiling. Output lines are "name ns/op ceiling status".
set -euo pipefail
cd "$(dirname "$0")/.."
status=0
while read -r name ceiling; do
    [[ -z "$name" || "$name" == \#* ]] && continue
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
