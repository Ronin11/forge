#!/bin/bash
# writes the right answer and a file with trailing whitespace a formatter
# strips, so a mutating `fmt` after it has something to commit
source "$(dirname "$0")/lib.sh"
cat >/dev/null
echo 42 > answer.txt
printf 'hello   \n' > notes.txt
git add -A && git commit -qm "answer, unformatted"
result "wrote the answer" answer.txt:added notes.txt:added
