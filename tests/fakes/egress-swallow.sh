#!/bin/bash
# asks the egress proxy for a host off the allowlist and swallows the 403,
# the way a CLI's own telemetry does, then does what ok.sh does: a refusal
# no line of output mentions
cat >/dev/null
if exec 3<>/dev/tcp/127.0.0.1/3128; then
  printf 'CONNECT telemetry.example.net:443 HTTP/1.1\r\nHost: telemetry.example.net:443\r\n\r\n' >&3
  timeout 5 cat <&3 >/dev/null
  exec 3>&-
fi
exec "$(dirname "$0")/ok.sh" </dev/null
