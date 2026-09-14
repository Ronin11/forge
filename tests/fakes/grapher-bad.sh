#!/bin/bash
# a map that names a file that does not exist
source "$(dirname "$0")/lib.sh"
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
result "mapped" docs/SYSTEM.md:added
