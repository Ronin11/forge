#!/bin/bash
# Assert the Agent launch still gets its settings and inherited provider keys.
set -eu
test "$(cat "$CLAUDE_CONFIG_DIR/settings.json")" = operator-settings
test ! -e "$CLAUDE_CONFIG_DIR/real-secret.txt"
test "$ANTHROPIC_API_KEY" = operator-anthropic
test "$CODEX_API_KEY" = operator-codex
test "$COPILOT_GITHUB_TOKEN" = operator-copilot
test -n "$CODEX_HOME"
test -n "$COPILOT_HOME"
exec "$(dirname "$0")/ok.sh" "$@"
