#!/bin/bash
# The claude CLI of 2026-09-26 23:48: its OAuth session had expired and
# could not refresh, so every launch, the kernel's login probe included,
# exits in half a second with this result frame and nothing else. See
# src/login_hold.rs.
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","num_turns":0,"total_cost_usd":0,"result":"Failed to authenticate: OAuth session expired and could not be refreshed","session_id":"s"}'
exit 1
