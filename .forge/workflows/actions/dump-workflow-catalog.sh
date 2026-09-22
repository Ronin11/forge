#!/usr/bin/env bash
# The author-workflow job's first step: a condensed dump of the action
# catalog and every workflow `forge workflows --json` reports (name, kind,
# description, what each action consumes/produces, each workflow's trigger),
# followed by one existing build workflow and one existing run workflow in
# full, as examples of the exact TOML shape. `produces = ["interface"]` on
# this action (dump-workflow-catalog.toml) is what makes this step's stdout
# the next directive step's input, the way an operation's stdout is any
# code step's interface (docs/JOBS.md, "Steps").
set -uo pipefail

forge_bin="forge"
[ -n "${FORGE_BIN_DIR:-}" ] && forge_bin="$FORGE_BIN_DIR/forge"

catalog=$("$forge_bin" workflows --json 2>/dev/null)
if [ -z "$catalog" ]; then
  echo "forge workflows --json produced no output" >&2
  exit 1
fi

echo "## Action and workflow catalog (condensed)"
echo '```json'
echo "$catalog" | jq '{
  actions: [.actions[] | {name, kind, contract, description, consumes, produces}],
  workflows: [.workflows[] | {name, kind, description, trigger}]
}'
echo '```'
echo

home="${FORGE_HOME:-$HOME/.local/share/forge}"
build_example="$home/workflows/direct.toml"
if [ -f "$build_example" ]; then
  echo "## Example: an existing build workflow (kind = \"build\" is the default)"
  echo '```toml'
  cat "$build_example"
  echo '```'
  echo
fi

run_example=".forge/workflows/changelog-line.toml"
if [ -f "$run_example" ]; then
  echo "## Example: an existing run workflow in this repository"
  echo '```toml'
  cat "$run_example"
  echo '```'
fi
