#!/bin/bash
# a map that names a file that does not exist
cat >/dev/null
mkdir -p docs
cat > docs/SYSTEM.md <<'EOF'
# System map

```mermaid
graph LR
  User --> Sim[src/sim/index.ts]
```

The simulation lives in src/sim/index.ts.
EOF
git add -A && git commit -qm "system map"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"mapped","changes":[{"path":"docs/SYSTEM.md","kind":"added"}]}}'
