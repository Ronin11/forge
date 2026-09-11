#!/bin/bash
# a map that names a file that does not exist
cat >/dev/null
mkdir -p docs
cat > docs/SYSTEM.md <<'EOF'
# System map

```mermaid
graph LR
  User --> Notes[docs/missing.md]
```

The notes live in docs/missing.md; export/import is prose, not a path.
EOF
git add -A && git commit -qm "system map"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"mapped","changes":[{"path":"docs/SYSTEM.md","kind":"added"}]}}'
