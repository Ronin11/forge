#!/usr/bin/env bash
# Release notes for a tag, from the commits since the previous tag. Same
# grouping as the site's patch notes (site/tools/patch-notes.py): feat, fix,
# perf, refactor are entries; everything else is counted as housekeeping.
#
#   scripts/release-notes.sh v0.2.0 > notes.md
set -euo pipefail
cd "$(dirname "$0")/.."

tag="${1:?usage: release-notes.sh <tag>}"
prev="$(git describe --tags --abbrev=0 "$tag^" 2>/dev/null || true)"
range="${prev:+$prev..}$tag"

section() { # kind heading
    local lines
    lines="$(git log --no-merges --format='%s' "$range" | grep -E "^$1(\([^)]*\))?!?: " | sed -E "s/^$1(\([^)]*\))?!?: /- /" || true)"
    [ -n "$lines" ] || return 0
    printf '## %s\n\n%s\n\n' "$2" "$lines"
}

if [ -n "$prev" ]; then
    section feat "Features"
    section fix "Fixes"
    section perf "Performance"
    section refactor "Refactoring"
    other=$(git log --no-merges --format='%s' "$range" | grep -cvE '^(feat|fix|perf|refactor)(\([^)]*\))?!?: ' || true)
    [ "$other" -gt 0 ] && printf '_%s housekeeping commits (docs, tests, chores)._\n\n' "$other"
else
    # First tag: the whole history would be hundreds of lines. Counts only.
    total=$(git rev-list --count --no-merges "$tag")
    printf 'First tagged release: %s commits since the repository began. The day-by-day history is at https://crashbyforge.com/notes.html.\n\n' "$total"
fi

cat <<MD
## Install

| Platform | Download |
| --- | --- |
| Linux x86_64 | [forge_linux_amd64.tar.gz](https://github.com/Ronin11/forge/releases/download/$tag/forge_linux_amd64.tar.gz) |
| Linux arm64 | [forge_linux_arm64.tar.gz](https://github.com/Ronin11/forge/releases/download/$tag/forge_linux_arm64.tar.gz) |
| macOS Apple silicon | [forge_darwin_arm64.tar.gz](https://github.com/Ronin11/forge/releases/download/$tag/forge_darwin_arm64.tar.gz) |
| macOS Intel | [forge_darwin_amd64.tar.gz](https://github.com/Ronin11/forge/releases/download/$tag/forge_darwin_amd64.tar.gz) |

Unpack and put \`forge\` on your PATH; \`forge version\` prints \`$tag\`. macOS
binaries are unsigned — clear the quarantine bit after a browser download with
\`xattr -d com.apple.quarantine forge\`. Windows: run the Linux build under WSL2
(a native build is tracked in docs/RELEASING.md). \`checksums.txt\` holds
SHA-256 sums for every archive.
MD

if [ -n "$prev" ]; then
    printf '\n**Full changelog**: https://github.com/Ronin11/forge/compare/%s...%s\n' "$prev" "$tag"
fi
