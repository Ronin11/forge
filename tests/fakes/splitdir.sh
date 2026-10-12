#!/bin/bash
# the task that says "split" moves a/'s files into two other directories, two
# each, so no destination has a majority; the one that says "add" puts a new
# file in a/. Merged, they make merge-tree exit 1 with no conflicted file.
source "$(dirname "$0")/lib.sh"
prompt=$(cat)
if grep -q 'split a' <<<"$prompt"; then
  mkdir b c
  git mv a/w b/w && git mv a/x b/x && git mv a/y c/y && git mv a/z c/z
  git commit -qm "split a"
  result "split a" b/w:added c/y:added
else
  echo new > a/new.txt; git add -A && git commit -qm "add to a"
  result "added to a" a/new.txt:added
fi
