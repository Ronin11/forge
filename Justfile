# Forge developer tasks. `just check` is the gate at every milestone boundary.

set shell := ["bash", "-euo", "pipefail", "-c"]

staticcheck_version := "v0.8.1"
errcheck_version := "v1.20.0"

# Build the binary into ./forge with the git describe as its version.
build:
    go build -ldflags "-X main.version=$(git describe --tags --always --dirty)" -o forge ./cmd/forge

# Run the control plane and a worker in one process (M1).
run: build
    ./forge run

# gofmt must report nothing.
fmt-check:
    test -z "$(gofmt -l cmd internal 2>/dev/null)" || (echo "gofmt needed:"; gofmt -l cmd internal; exit 1)

vet:
    go vet ./...

staticcheck:
    go run honnef.co/go/tools/cmd/staticcheck@{{staticcheck_version}} ./...

errcheck:
    go run github.com/kisielk/errcheck@{{errcheck_version}} -blank -asserts -exclude .errcheck_excludes ./...

# Race detector always on; tests use real git repos and real child processes.
test:
    go test -race -count=1 ./...

# Benchmarks on the event path and the stats query with a regression threshold
# recorded in bench/threshold.txt (added in M1 with the first benchmark).
bench:
    @if [ -f bench/threshold.txt ]; then ./scripts/bench-check.sh; else echo "bench: no benchmarks yet (M1)"; fi

# Layering, proved by go list: the worker never imports controlplane or store (one
# SQLite writer); model and protocol import nothing of Forge. grep is not run with -q
# so a SIGPIPE cannot turn a violation into a pass under pipefail.
boundary:
    @for p in $(go list ./internal/worker/... 2>/dev/null); do \
        if go list -deps "$p" | grep -E '^forge/internal/(controlplane|store)(/|$)' >/dev/null; then echo "boundary: $p imports controlplane or store"; exit 1; fi; \
    done
    @for p in $(go list ./internal/model/... ./internal/protocol/... 2>/dev/null); do \
        if go list -deps "$p" | grep -E '^forge/' | grep -vx "$p" >/dev/null; then echo "boundary: $p imports another Forge package"; exit 1; fi; \
    done
    @echo "boundary: ok"

# Knowledge-base integrity (M2): dangling links, bad frontmatter, id mismatches.
kb-check: build
    @if [ -x ./forge ] && ./forge help 2>/dev/null | grep -q '^  kb'; then ./forge kb check; else echo "kb-check: not yet (M2)"; fi

# Playwright tests against the Forge UI (M4). Node is a test-time dependency only.
ui-test:
    @if [ -f ui/package.json ]; then cd ui && npm test; else echo "ui-test: not yet (M4)"; fi

# Line counts for milestone reports.
lines:
    @printf "Go (non-test): "; (find cmd internal -name '*.go' ! -name '*_test.go' 2>/dev/null | xargs cat 2>/dev/null || true) | wc -l
    @printf "Go (test):     "; (find cmd internal -name '*_test.go' 2>/dev/null | xargs cat 2>/dev/null || true) | wc -l
    @printf "UI (tmpl/js/css): "; (find internal -path '*/ui/*' -type f \( -name '*.html' -o -name '*.js' -o -name '*.css' \) 2>/dev/null | xargs cat 2>/dev/null || true) | wc -l

# The gate.
check: fmt-check vet staticcheck errcheck boundary test bench kb-check ui-test lines
    @echo "check: green"
