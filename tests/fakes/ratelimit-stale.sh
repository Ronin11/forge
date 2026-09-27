#!/bin/bash
# Refuses every run with a rate_limit_event whose window already reset (a
# reset time slightly in the past): window_hold never holds on it, so
# nothing but engine::run_directive_step's own refusal bound
# (docs/REVIEW-3.md item 1.1.4) stops it from spinning the slot forever.
cat >/dev/null
r=$(( $(date +%s) - 10 ))
echo '{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":'"$r"',"unifiedWindows":{"five_hour":{"utilization":1.0,"resetsAt":'"$r"'},"seven_day":{"utilization":0.2,"resetsAt":1800500000}}}}'
echo '{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":0,"total_cost_usd":0,"result":"You have hit your rate limit","session_id":"s"}'
exit 1
