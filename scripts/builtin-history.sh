#!/usr/bin/env bash
# Regenerate src/builtins/history.tsv: for every built-in action and
# operation file, one line per blob hash it has had in this repository's
# history: <file> TAB <unix time of the commit that made it> TAB <hash>.
# build.rs compiles the table into the binary, because a release built by
# `forge deploy` comes from a `git archive` tree with no .git and cannot ask
# git (docs/DEPLOY.md). Run it after editing a built-in file; a file whose
# current text is not in the log yet is stamped with the time of the run.
# tests: `builtin_history_lists_every_built_in_current_text` fails until then.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
out=src/builtins/history.tsv
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

git log --raw --no-abbrev --no-renames --format=@%ct -- \
    src/builtins/actions src/builtins/operations |
    awk '
        /^@/ { when = substr($0, 2); next }
        /\t/ {
            split($0, parts, "\t")
            n = split(parts[1], meta, " ")
            hash = meta[4]
            m = split(parts[2], path, "/")
            if (hash !~ /^0+$/) print path[m] "\t" when "\t" hash
        }' >"$tmp"

now="$(date +%s)"
for f in src/builtins/actions/*.toml src/builtins/operations/*.toml; do
    hash="$(git hash-object "$f")"
    name="$(basename "$f")"
    if ! grep -q "^${name}	[0-9]*	${hash}\$" "$tmp"; then
        printf '%s\t%s\t%s\n' "$name" "$now" "$hash" >>"$tmp"
    fi
done

sort -t "$(printf '\t')" -k1,1 -k2,2n -k3,3 -u "$tmp" >"$out"
