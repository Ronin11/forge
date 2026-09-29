#!/bin/bash
# first run: the provider refuses it (5h window at 100%, resets in 3s); next run: a confirming review
source "$(dirname "$0")/lib.sh"
cat >/dev/null
# Keep provider state across verification, which replaces Git metadata.
rl_dir="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
mkdir -p "$rl_dir"
rl_key=$(printf '%s' "$PWD" | sha256sum | cut -d ' ' -f 1)
rl_marker="$rl_dir/forge-test-$rl_key.rl-seen"
if [ ! -f "$rl_marker" ]; then
  touch "$rl_marker"
  r=$(( $(date +%s) + 3 ))
  echo '{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":'"$r"',"unifiedWindows":{"five_hour":{"utilization":1.0,"resetsAt":'"$r"'},"seven_day":{"utilization":0.2,"resetsAt":1800500000}}}}'
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":0,"total_cost_usd":0,"result":"You have hit your rate limit","session_id":"s"}'
  exit 1
fi
rm -f "$rl_marker"
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"npm test"}}]}}'
result "Ran the answer check and inspected answer.txt; the change does what the task asked."
