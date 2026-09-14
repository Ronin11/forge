#!/bin/bash
# first run: the provider refuses it (window exhausted, resets in 2s); next run: fine
source "$(dirname "$0")/lib.sh"
cat >/dev/null
if [ ! -f "$(pwd)/.rl-seen" ]; then
  touch "$(pwd)/.rl-seen"
  r=$(( $(date +%s) + 2 ))
  echo '{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":'"$r"',"unifiedWindows":{"five_hour":{"utilization":1.0,"resetsAt":'"$r"'},"seven_day":{"utilization":0.2,"resetsAt":1800500000}}}}'
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":0,"total_cost_usd":0,"result":"You have hit your rate limit","session_id":"s"}'
  exit 1
fi
rm -f "$(pwd)/.rl-seen"
echo '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","unifiedWindows":{"five_hour":{"utilization":0.3,"resetsAt":1800000000},"seven_day":{"utilization":0.2,"resetsAt":1800500000}}}}'
echo 42 > answer.txt && git add -A && git commit -qm "answer"
result "wrote the answer" answer.txt:added
