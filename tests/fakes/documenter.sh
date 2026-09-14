#!/bin/bash
# the document step done right: a comment and a doc, nothing else
source "$(dirname "$0")/lib.sh"
cat >/dev/null
sed -i '1a # prints a greeting' hello.sh
mkdir -p docs && printf '# Notes\n\nhello.sh prints a greeting.\n' > docs/NOTES.md
git add -A && git commit -qm "document the greeting"
result "documented" hello.sh:modified docs/NOTES.md:added
