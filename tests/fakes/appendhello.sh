#!/bin/bash
source "$(dirname "$0")/lib.sh"
cat >/dev/null
echo 'echo extra' >> hello.sh
git add hello.sh && git commit -qm 'extend hello'
result 'extended hello' hello.sh:modified
