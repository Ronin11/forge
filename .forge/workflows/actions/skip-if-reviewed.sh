#!/usr/bin/env bash
# [skip_if] for engineering-weekly: exit 0 (skip) iff a task whose text
# starts with "docs/REVIEW" already landed (succeeded) in the last 7
# days — a review done by hand or by an earlier run of this job satisfies
# the week. Exit non-zero otherwise, so the job proceeds and measures.
set -uo pipefail

forge_bin="forge"
[ -n "${FORGE_BIN_DIR:-}" ] && forge_bin="$FORGE_BIN_DIR/forge"

now=$(date +%s)
week_ago=$((now - 7 * 24 * 3600))

"$forge_bin" log --json --project "$FORGE_PROJECT" --state succeeded --grep "docs/REVIEW" --limit 200 \
  | jq -e --argjson since "$week_ago" '
      any(.[]; (.text // "" | startswith("docs/REVIEW")) and ((.finished_at // 0) >= $since))
    ' >/dev/null
