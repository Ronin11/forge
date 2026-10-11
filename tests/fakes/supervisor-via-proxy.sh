#!/bin/bash
# the supervisor reaches its provider's model endpoint (ANTHROPIC_BASE_URL)
# through the sandbox's egress proxy, as the claude CLI does, then settles
# the question the way supervisor-answer.sh does; a tunnel the proxy refuses
# fails the run the way the CLI does
cat >/dev/null
endpoint=${ANTHROPIC_BASE_URL#http://}
endpoint=${endpoint%%/*}
reply=
if [ -n "$HTTPS_PROXY" ] && exec 3<>/dev/tcp/127.0.0.1/3128; then
  printf 'CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n' "$endpoint" "$endpoint" >&3
  read -r -t 10 status <&3
  case "$status" in
    *" 200 "*)
      while read -r -t 10 line <&3 && [ -n "${line%$'\r'}" ]; do :; done
      read -r -t 10 reply <&3
      ;;
  esac
fi
if [ "${reply%$'\r'}" != "fake-model-ok" ]; then
  echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"num_turns\":1,\"total_cost_usd\":0,\"result\":\"API Error: Couldn't connect through your proxy (ERR_PROXY_TUNNEL)\"}"
  exit 1
fi
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.05,"result":"ruled","structured_output":{"action":"answer","reason":"the repository already uses lowercase file names","answer":"Use answer.txt, lowercase, at the repository root: every existing file here is lowercase (see hello.sh) and the check reads answer.txt.","citations":["hello.sh","forge.toml"],"prerequisite":null}}'
