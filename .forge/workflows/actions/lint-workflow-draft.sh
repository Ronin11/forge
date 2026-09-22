#!/usr/bin/env bash
# The author-workflow job's last step: the draft-workflow directive's
# structured output ($FORGE_OUTPUT_DRAFT_WORKFLOW) holds a candidate
# workflow file's whole text in its `toml` field. Write that text to a
# scratch path under the job's input directory and lint it with the same
# parser the operator's catalog uses (`forge workflows lint --stdin`),
# failing this step — and so the job — with the lint output when it does
# not pass. A job that ends `ok` therefore always drafted a workflow file
# that lints clean; one that ends `failed` names exactly what is wrong,
# for `on_failure = "ask:operator"` to ask about.
set -uo pipefail

out="$FORGE_OUTPUT_DRAFT_WORKFLOW"
if [ -z "${out:-}" ] || [ ! -f "$out" ]; then
  echo "no draft-workflow output to lint" >&2
  exit 1
fi

name=$(jq -r '.name' "$out")
if [ -z "$name" ] || [ "$name" = "null" ]; then
  echo "the draft has no name" >&2
  exit 1
fi

scratch="$FORGE_INPUT_DIR/draft-${name}.toml"
jq -r '.toml' "$out" > "$scratch"

forge_bin="forge"
[ -n "${FORGE_BIN_DIR:-}" ] && forge_bin="$FORGE_BIN_DIR/forge"

if ! problems=$("$forge_bin" workflows lint --stdin --name "$name" < "$scratch"); then
  echo "$problems"
  echo "draft workflow ${scratch} failed lint" >&2
  exit 1
fi

echo "$problems"
echo "draft workflow ${scratch} lints clean"
