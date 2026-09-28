#!/bin/bash
# Ask Forge on the operator's default provider (a claude CLI): the whole
# prompt arrives on stdin, the directive and the tool list included. The
# answer is a finished reply that quotes the first line of the directive,
# so the test can see the model was told who it is.
prompt=$(cat)
case "$prompt" in
  *"You are Ask Forge"*"- retry_task:"*) reply="told who I am and what I can use" ;;
  *) reply="the directive never reached me" ;;
esac
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.002,"result":"done","structured_output":{"reply":"'"$reply"'","tool":"","arguments":{}}}'
