#!/usr/bin/env bash
# The engineering-weekly job's second step: compare measurements.json
# (written by measure.sh, in the same scratch) against the thresholds the
# workflow file declares as [env] — never hard-coded here. When one is
# crossed, file a task on the project through `forge add` carrying the
# measurements and the crossed threshold, asking for a review in the
# shape of docs/REVIEW-2.md. A dry run only logs what it would have
# filed: `forge add` itself is never called.
set -uo pipefail

measurements="measurements.json"
if [ ! -f "$measurements" ]; then
  printf 'row\tthresholds\tno measurements.json to compare\n' >> "$FORGE_EFFECT_LOG"
  exit 0
fi

run_task_max=${RUN_TASK_MAX_LINES:-400}
src_file_max=${SRC_FILE_MAX_LINES:-3000}
repo="${FORGE_REPO_DIR:-.}"

run_task_lines=$(awk '
  /^[[:space:]]*(pub([(][^)]*[)])?[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+run_task[[:space:]]*\(/ {
    if (depth == 0) { start = NR; capturing = 1 }
  }
  capturing {
    n = gsub(/{/, "{"); depth += n
    m = gsub(/}/, "}"); depth -= m
    if (depth == 0 && (n + m) > 0 && NR >= start) {
      print (NR - start + 1)
      capturing = 0
      exit
    }
  }
' "$repo/src/engine.rs" 2>/dev/null)
run_task_lines=${run_task_lines:-0}

crossed=""
if [ "$run_task_lines" -gt "$run_task_max" ] 2>/dev/null; then
  crossed="run_task is ${run_task_lines} lines (over ${run_task_max})"
fi

if [ -z "$crossed" ]; then
  big=$(jq -r --argjson max "$src_file_max" '.largest_files[]? | select(.lines > $max) | "\(.path) (\(.lines) lines)"' "$measurements" 2>/dev/null | head -1)
  if [ -n "$big" ]; then
    crossed="a src file over ${src_file_max} lines: ${big}"
  fi
fi

if [ -z "$crossed" ]; then
  missing=$(jq -r '.modules_without_tests[0] // empty' "$measurements" 2>/dev/null)
  if [ -n "$missing" ]; then
    crossed="a module without a #[cfg(test)] block: ${missing}"
  fi
fi

if [ -z "$crossed" ]; then
  warnings=$(jq -r '.clippy_warnings // 0' "$measurements" 2>/dev/null)
  if [ "$warnings" != "null" ] && [ "$warnings" -gt 0 ] 2>/dev/null; then
    crossed="${warnings} clippy warning(s)"
  fi
fi

if [ -z "$crossed" ]; then
  printf 'row\tthresholds\tno threshold crossed\n' >> "$FORGE_EFFECT_LOG"
  exit 0
fi

measurements_text=$(tr -d '\n' < "$measurements")
text="docs/REVIEW-3.md: engineering-weekly crossed a threshold — ${crossed}. Write a review document in the shape of docs/REVIEW-2.md from this week's measurements: ${measurements_text}"

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
