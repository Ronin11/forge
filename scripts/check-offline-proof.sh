#!/usr/bin/env bash
# M8 offline proof (STYLE §11): `just check` must pass with no network and no
# `claude` binary on PATH. Builds a minimal PATH of symlinks that excludes
# claude, disables the network with `unshare -rn` when unprivileged user
# namespaces allow it (documented fallback otherwise), and times the run.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=.scratch/offline-bin
rm -rf "$BIN"
mkdir -p "$BIN"

# Everything `just check` needs, and nothing else. claude is deliberately
# absent; node/npm/npx are present only so ui-test can take its offline path
# (cached chromium) or print its skip line.
tools=(bash sh env just go gofmt git grep sed awk find xargs cat wc ls cp mv rm
       mkdir rmdir test true false printf echo head tail tee sort uniq cut tr
       date uname dirname basename readlink chmod touch diff mktemp od sleep
       node npm npx
       gcc cc as ld ar) # cgo toolchain: `go test -race` requires cgo
for t in "${tools[@]}"; do
    if p=$(type -P "$t" 2>/dev/null) && [ -n "$p" ]; then # type -P: builtins like `true` need the real binary
        ln -sf "$p" "$BIN/$t"
    fi
done
# gofmt may live in GOROOT/bin rather than on PATH.
if [ ! -e "$BIN/gofmt" ] && [ -x "$(go env GOROOT)/bin/gofmt" ]; then
    ln -sf "$(go env GOROOT)/bin/gofmt" "$BIN/gofmt"
fi

ABS_BIN=$PWD/$BIN
echo "offline PATH: $ABS_BIN"
if PATH=$ABS_BIN command -v claude >/dev/null 2>&1; then
    echo "FAIL: claude is reachable on the reduced PATH" >&2
    exit 1
fi
echo "claude on reduced PATH: absent (good)"

# The file:// GOPROXY serves `go run pkg@version` (goimports, staticcheck,
# errcheck in the Justfile) from the local module cache with zero network; the
# trailing `,off` makes anything not already cached a hard error rather than a
# download. GOSUMDB=off stops the sum-db lookup those ad-hoc runs would try.
runner=(env -i PATH="$ABS_BIN" HOME="$HOME" CGO_ENABLED=1
        GOPROXY="file://$(go env GOMODCACHE)/cache/download,off" GOSUMDB=off
        TERM="${TERM:-dumb}" LANG="${LANG:-C.UTF-8}")
UNSHARE=$(command -v unshare || true)
IPBIN=$(command -v ip || true)
if [ -n "$UNSHARE" ] && "$UNSHARE" -rn true 2>/dev/null; then
    # Loopback starts DOWN in a fresh netns; the tests bind 127.0.0.1, and the
    # -r root mapping lets us bring lo up. External hosts stay unreachable.
    echo "network: disabled with unshare -rn (loopback up inside the namespace)"
    runner+=(IPBIN="$IPBIN" "$UNSHARE" -rn /bin/sh -c
             '[ -n "$IPBIN" ] && "$IPBIN" link set lo up 2>/dev/null; exec just check')
else
    echo "network: unshare -rn unavailable (needs unprivileged user namespaces);"
    echo "         relying on the file://+off GOPROXY and the reduced PATH only"
    runner+=(just check)
fi

start=$(date +%s)
set +e
"${runner[@]}"
status=$?
set -e
end=$(date +%s)
echo "offline just check: exit=$status runtime=$((end - start))s"
exit $status
