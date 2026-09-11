#!/bin/bash
# the graph step: a map with a mermaid block naming real things
cat >/dev/null
mkdir -p docs
cat > docs/SYSTEM.md <<'EOF'
# System map

```mermaid
graph LR
  User --> hello[hello.sh]
  hello --> Answer[answer.txt]
```

`hello.sh` is the entry point. `answer.txt` holds the answer; see docs/SYSTEM.md for this map.
EOF
git add -A && git commit -qm "system map"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"mapped","changes":[{"path":"docs/SYSTEM.md","kind":"added"}]}}'
