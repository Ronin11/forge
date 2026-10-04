#!/usr/bin/env bash
# The engineering-weekly job's second step: compare measurements.json
# (written by measure.sh, in the same scratch) against the thresholds the
# workflow file declares as [env] — never hard-coded here. When one is
# crossed, file one task on the project through `forge add` naming every
# crossing and carrying the measurements, asking for a review in the
# shape of docs/REVIEW-2.md. A dry run only logs what it would have
# filed: `forge add` itself is never called.
set -uo pipefail

measurements="measurements.json"
if [ ! -f "$measurements" ]; then
  printf 'row\tthresholds\tno measurements.json to compare\n' >> "$FORGE_EFFECT_LOG"
  exit 0
fi

function_max=${FUNCTION_MAX_LINES:-${RUN_TASK_MAX_LINES:-400}}
src_file_max=${SRC_FILE_MAX_LINES:-3000}

# Every crossing is collected, never just the first: one filed task
# names them all.
crossings=()

while IFS= read -r line; do
  [ -n "$line" ] && crossings+=("$line")
done < <(jq -r --argjson max "$function_max" '
  .longest_functions[]? | select(.lines > $max) |
    "\(.at) \(.signature) is \(.lines) lines (over \($max))"
' "$measurements" 2>/dev/null)

while IFS= read -r line; do
  [ -n "$line" ] && crossings+=("$line")
done < <(jq -r --argjson max "$src_file_max" '
  .largest_files[]? | select(.lines > $max) |
    "src file \(.path) is \(.lines) lines (over \($max))"
' "$measurements" 2>/dev/null)

while IFS= read -r line; do
  [ -n "$line" ] && crossings+=("$line")
done < <(jq -r '
  .modules_without_tests[]? | "module without a #[cfg(test)] block: \(.)"
' "$measurements" 2>/dev/null)

warnings=$(jq -r '.clippy_warnings // 0' "$measurements" 2>/dev/null)
if [ "$warnings" != "null" ] && [ "$warnings" -gt 0 ] 2>/dev/null; then
  crossings+=("${warnings} clippy warning(s)")
fi

if [ "${#crossings[@]}" -eq 0 ]; then
  printf 'row\tthresholds\tno threshold crossed\n' >> "$FORGE_EFFECT_LOG"
  exit 0
fi

crossed=""
for c in "${crossings[@]}"; do
  crossed="${crossed:+$crossed; }$c"
done

measurements_text=$(tr -d '\n' < "$measurements")
text="engineering-weekly crossed ${#crossings[@]} threshold(s): ${crossed}. Write a review document in the shape of docs/REVIEW-2.md from this week's measurements: ${measurements_text}"

if [ "${FORGE_DRY_RUN:-}" = "1" ]; then
  printf 'row\ttask\t%s (dry run)\n' "$text" >> "$FORGE_EFFECT_LOG"
  exit 0
fi

forge_bin="forge"
[ -n "${FORGE_BIN_DIR:-}" ] && forge_bin="$FORGE_BIN_DIR/forge"

add_out=$(mktemp)
if "$forge_bin" add "$FORGE_REPO_DIR" "$text" >"$add_out" 2>&1; then
  printf 'row\ttask\t%s\n' "$text" >> "$FORGE_EFFECT_LOG"
  rm -f "$add_out"
else
  cat "$add_out" >&2
  rm -f "$add_out"
  exit 1
fi
