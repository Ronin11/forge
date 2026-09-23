#!/bin/bash
# Answers right only when forge-test is on PATH inside the sandbox and runs
# the declared test check of a fake repository, then answers the same tree
# from its cache.
source "$(dirname "$0")/lib.sh"
cat >/dev/null
fake=$(mktemp -d)
git init -q "$fake"
printf '[checks]\ntest = ["sh", "-c", "echo test result: ok. 1 passed"]\n' > "$fake/forge.toml"
first=$(cd "$fake" && forge-test)
second=$(cd "$fake" && forge-test)
if echo "$first" | grep -q "exit 0" && echo "$first" | grep -q "1 passed" \
  && echo "$second" | grep -q "cached: tree unchanged"; then
  echo 42 > answer.txt
else
  echo 41 > answer.txt
fi
git add -A && git commit -qm "answer"
result "x" answer.txt:added
