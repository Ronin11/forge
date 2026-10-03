#!/usr/bin/env bash
# Run from the repository root: bash tests/review-notes/949/dead-plugin-pid.sh
# An enabled plugin whose recorded `running` pid is dead, while a worker is
# live: the plugins row must describe it as "stopped: supervisor gone"
# everywhere, never "running pid N, up ...".
set -euo pipefail
cargo build -q --bin forge
B=$PWD/target/debug/forge
T=$(mktemp -d); export FORGE_HOME=$T/home
"$B" doctor >/dev/null 2>&1 || true
sqlite3 "$FORGE_HOME/forge.db" "INSERT INTO plugins(name,enabled,enabled_at) VALUES('b',1,1)"
mkdir -p "$FORGE_HOME/plugins-run"
echo '{"state":"running","pid":2147483646,"since":1}' > "$FORGE_HOME/plugins-run/b.json"
sleep 60 & P=$!
sqlite3 "$FORGE_HOME/forge.db" "INSERT INTO workers(pid,version,started_at) VALUES($P,'r1',1)"
row=$("$B" doctor 2>&1 | grep '^[A-Z]* *plugins' || true)
kill $P; rm -rf "$T"
echo "$row"
if grep -q 'running pid 2147483646' <<<"$row"; then
  echo "DEFECT: the dead pid still reads as running in the plugins row"; exit 1
fi
echo "ok"
