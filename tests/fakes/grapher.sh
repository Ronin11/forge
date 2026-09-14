#!/bin/bash
# the graph step: a map with a mermaid block naming real things
source "$(dirname "$0")/lib.sh"
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
result "mapped" docs/SYSTEM.md:added
