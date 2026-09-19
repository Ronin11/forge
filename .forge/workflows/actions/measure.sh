#!/usr/bin/env bash
# The engineering-weekly job's first step: measure the tree the way
# docs/REVIEW-2.md opens (kernel lines, the five largest files, the eight
# longest functions, modules without a #[cfg(test)] block, test counts,
# e2e wall time, clippy warnings), write it to measurements.json in the
# scratch, and log it as a row effect. Degrades to null/empty fields
# rather than failing when a tool (cargo, clippy) or the tree (no src/)
# isn't there to ask, so a dry run or a minimal fixture still measures.
set -uo pipefail

repo="${FORGE_REPO_DIR:-$PWD}"
out="measurements.json"

kernel_files() {
  [ -d "$repo/src" ] && find "$repo/src" -name '*.rs' 2>/dev/null | sort
}

kernel_lines=0
largest_files_json="[]"
longest_functions_json="[]"
modules_without_tests_json="[]"

if [ -d "$repo/src" ]; then
  files_tmp=$(mktemp)
  funcs_tmp=$(mktemp)
  notests_tmp=$(mktemp)

  while IFS= read -r f; do
    [ -z "$f" ] && continue
    rel=${f#"$repo"/}
    n=$(wc -l < "$f" 2>/dev/null | tr -d ' ')
    n=${n:-0}
    kernel_lines=$((kernel_lines + n))
    printf '%s\t%s\n' "$n" "$rel" >> "$files_tmp"

    # Longest functions: an approximate brace-depth scan. Good enough for
    # a weekly measurement, not a substitute for reading the tree.
    awk -v path="$rel" '
      /^[[:space:]]*(pub([(][^)]*[)])?[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+[A-Za-z_][A-Za-z0-9_]*/ {
        if (depth == 0) {
          start = NR
          name = $0
          sub(/^[[:space:]]*/, "", name)
          sub(/[[:space:]]*[{]?[[:space:]]*$/, "", name)
          capturing = 1
        }
      }
      capturing {
        n = gsub(/{/, "{"); depth += n
        m = gsub(/}/, "}"); depth -= m
        if (depth == 0 && (n + m) > 0 && NR >= start) {
          printf "%d\t%s:%d\t%s\n", (NR - start + 1), path, start, name
          capturing = 0
        }
      }
    ' "$f" >> "$funcs_tmp"

    if ! grep -q '#\[cfg(test)\]' "$f" 2>/dev/null; then
      printf '%s\n' "$rel" >> "$notests_tmp"
    fi
  done < <(kernel_files)

  if [ -s "$files_tmp" ]; then
    largest_files_json=$(sort -t "$(printf '\t')" -k1 -rn "$files_tmp" | head -5 | jq -R -s '
      [splits("\n") | select(length > 0) | split("\t") | {path: .[1], lines: (.[0] | tonumber)}]
    ')
  fi
  if [ -s "$funcs_tmp" ]; then
    longest_functions_json=$(sort -t "$(printf '\t')" -k1 -rn "$funcs_tmp" | head -8 | jq -R -s '
      [splits("\n") | select(length > 0) | split("\t") | {at: .[1], lines: (.[0] | tonumber), signature: .[2]}]
    ')
  fi
  if [ -s "$notests_tmp" ]; then
    modules_without_tests_json=$(jq -R -s '[splits("\n") | select(length > 0)]' "$notests_tmp")
  fi
  rm -f "$files_tmp" "$funcs_tmp" "$notests_tmp"
fi

unit_tests=0
e2e_tests=0
[ -d "$repo/src" ] && unit_tests=$(grep -rE '#\[(tokio::)?test\]' "$repo/src" 2>/dev/null | wc -l | tr -d ' ')
[ -d "$repo/tests/e2e" ] && e2e_tests=$(grep -rE '#\[(tokio::)?test\]' "$repo/tests/e2e" 2>/dev/null | wc -l | tr -d ' ')

e2e_wall_time_secs="null"
clippy_warnings="null"
if command -v cargo >/dev/null 2>&1 && [ -f "$repo/Cargo.toml" ]; then
  t0=$(date +%s)
  if command -v timeout >/dev/null 2>&1; then
    timeout 120 cargo test --manifest-path "$repo/Cargo.toml" --test e2e --quiet >/dev/null 2>&1
  else
    ( cd "$repo" && cargo test --test e2e --quiet >/dev/null 2>&1 )
  fi
  t1=$(date +%s)
  e2e_wall_time_secs=$((t1 - t0))

  clippy_out=$(mktemp)
  if command -v timeout >/dev/null 2>&1; then
    timeout 120 cargo clippy --manifest-path "$repo/Cargo.toml" --workspace --all-targets --message-format=json > "$clippy_out" 2>/dev/null
  else
    cargo clippy --manifest-path "$repo/Cargo.toml" --workspace --all-targets --message-format=json > "$clippy_out" 2>/dev/null
  fi
  clippy_warnings=$(jq -s '[.[] | select(.reason=="compiler-message" and .message.level=="warning")] | length' "$clippy_out" 2>/dev/null)
  clippy_warnings=${clippy_warnings:-null}
  rm -f "$clippy_out"
fi

jq -n \
  --argjson kernel_lines "$kernel_lines" \
  --argjson largest_files "$largest_files_json" \
  --argjson longest_functions "$longest_functions_json" \
  --argjson modules_without_tests "$modules_without_tests_json" \
  --argjson unit_tests "$unit_tests" \
  --argjson e2e_tests "$e2e_tests" \
  --argjson e2e_wall_time_secs "$e2e_wall_time_secs" \
  --argjson clippy_warnings "$clippy_warnings" \
  '{
    kernel_lines: $kernel_lines,
    largest_files: $largest_files,
    longest_functions: $longest_functions,
    modules_without_tests: $modules_without_tests,
    test_counts: {unit: $unit_tests, e2e: $e2e_tests},
    e2e_wall_time_secs: $e2e_wall_time_secs,
    clippy_warnings: $clippy_warnings
  }' > "$out"

printf 'row\tmeasurements.json\t%s\n' "$(tr -d '\n' < "$out")" >> "$FORGE_EFFECT_LOG"
