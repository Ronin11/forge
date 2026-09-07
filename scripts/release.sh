#!/usr/bin/env bash
# Build the release matrix into dist/: one tarball per target plus checksums.
#
#   scripts/release.sh [version]     version defaults to `git describe`
#   TARGETS="linux/amd64" scripts/release.sh v0.2.0
#
# Archive names carry no version (forge_linux_amd64.tar.gz) on purpose: GitHub
# serves the newest one at a stable URL —
#   https://github.com/Ronin11/forge/releases/latest/download/forge_linux_amd64.tar.gz
# — so the site and the install script never need updating for a release. The
# version lives inside the binary (`forge version`) and on the release tag.
#
# Pure Go (modernc sqlite), so CGO_ENABLED=0 cross-compiles every target from
# one Linux runner. Windows is not in the default list: the daemon's process
# model is POSIX (process groups, flock, exec-in-place restart, SIGUSR1,
# statfs) and does not compile there yet — see docs/RELEASING.md.
set -euo pipefail
cd "$(dirname "$0")/.."

version="${1:-$(git describe --tags --always --dirty)}"
targets="${TARGETS:-linux/amd64 linux/arm64 darwin/amd64 darwin/arm64}"

rm -rf dist && mkdir -p dist
for t in $targets; do
    os=${t%/*}; arch=${t#*/}
    name="forge_${os}_${arch}"
    stage="dist/.stage/$name"
    mkdir -p "$stage"
    echo "release: building $name ($version)"
    CGO_ENABLED=0 GOOS="$os" GOARCH="$arch" go build -trimpath \
        -ldflags "-s -w -X main.version=$version" -o "$stage/forge" ./cmd/forge
    cp README.md "$stage/"
    tar -C "$stage" -czf "dist/$name.tar.gz" forge README.md
done
rm -rf dist/.stage
(cd dist && sha256sum ./*.tar.gz | sed 's|\./||' > checksums.txt)
echo "release: dist/"
ls -l dist
