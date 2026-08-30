# Forge developer tasks. `just check` is the gate at every milestone boundary.

set shell := ["bash", "-euo", "pipefail", "-c"]

staticcheck_version := "v0.8.1"
errcheck_version := "v1.20.0"

# Build the binary into ./forge with the git describe as its version.
build:
    go build -ldflags "-X main.version=$(git describe --tags --always --dirty)" -o forge ./cmd/forge

# Run the daemon in the foreground (it spawns the worker) — M1.
run: build
    ./forge daemon start --foreground

# gofmt and goimports must report nothing (STYLE §10).
fmt-check:
    test -z "$(gofmt -l cmd internal 2>/dev/null)" || (echo "gofmt needed:"; gofmt -l cmd internal; exit 1)
    test -z "$(go run golang.org/x/tools/cmd/goimports@v0.44.0 -l cmd internal 2>/dev/null)" || (echo "goimports needed:"; go run golang.org/x/tools/cmd/goimports@v0.44.0 -l cmd internal; exit 1)

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
    @for p in $(go list ./internal/model/... 2>/dev/null); do \
        if go list -deps "$p" | grep -E '^forge/' | grep -vx "$p" >/dev/null; then echo "boundary: $p imports another Forge package"; exit 1; fi; \
    done
    @for p in $(go list ./internal/protocol/... 2>/dev/null); do \
        if go list -deps "$p" | grep -E '^forge/' | grep -vx "$p" | grep -v '^forge/internal/model$' >/dev/null; then echo "boundary: $p imports more than model"; exit 1; fi; \
    done
    @echo "boundary: ok"

# Knowledge-base integrity (M2): dangling links, bad frontmatter, id mismatches.
kb-check: build
    @if [ -x ./forge ] && ./forge help 2>/dev/null | grep -q '^  kb'; then ./forge kb check; else echo "kb-check: not yet (M2)"; fi

# Playwright tests against the Forge UI (M4). Node is a test-time dependency only.
# Offline skip (STYLE §11): when chromium is neither cached (~/.cache/ms-playwright)
# nor downloadable (npx playwright install chromium fails without network), or npm
# itself is unavailable, the recipe prints one skip line and exits 0 so `just check`
# stays green with no network and no browser download.
ui-test:
    @if [ ! -f ui/package.json ]; then echo "ui-test: not yet (M4)"; exit 0; fi; \
    if ! command -v npm >/dev/null 2>&1; then echo "ui-test: skipped (npm not available)"; exit 0; fi; \
    cd ui; \
    if [ ! -d node_modules ]; then \
        (npm ci --no-audit --no-fund >/dev/null 2>&1 || npm install --no-audit --no-fund >/dev/null 2>&1) \
            || { echo "ui-test: skipped (no chromium and offline)"; exit 0; }; \
    fi; \
    if ! ls "$HOME/.cache/ms-playwright"/chromium*/chrome-linux*/chrome >/dev/null 2>&1; then \
        npx playwright install chromium >/dev/null 2>&1 || true; \
        ls "$HOME/.cache/ms-playwright"/chromium*/chrome-linux*/chrome >/dev/null 2>&1 \
            || { echo "ui-test: skipped (no chromium and offline)"; exit 0; }; \
    fi; \
    npx playwright test

# Opt-in real-Claude smoke steps for the current milestone (spends budget; M1+).
smoke:
    @if [ -x ./scripts/smoke.sh ]; then ./scripts/smoke.sh; else echo "smoke: not yet (M1)"; fi

# Generated registries must be committed up to date (STYLE §10; M1+).
generate-check:
    @if grep -rq '^//go:generate' cmd internal 2>/dev/null; then go generate ./... && test -z "$(git status --porcelain -- '*_gen.go')" || (git status --porcelain -- '*_gen.go'; exit 1); else echo "generate-check: no generators yet (M1)"; fi

# STYLE §11: the gate must pass with no network and no claude binary.
check-offline:
    env PATH=/usr/bin:/bin:$(go env GOROOT)/bin HOME=$HOME unshare -Urn just check

# Line counts for milestone reports.
lines:
    @printf "Go (non-test): "; (find cmd internal -name '*.go' ! -name '*_test.go' 2>/dev/null | xargs cat 2>/dev/null || true) | wc -l
    @printf "Go (test):     "; (find cmd internal -name '*_test.go' 2>/dev/null | xargs cat 2>/dev/null || true) | wc -l
    @printf "UI (tmpl/js/css): "; (find internal -path '*/ui/*' -type f \( -name '*.html' -o -name '*.js' -o -name '*.css' \) 2>/dev/null | xargs cat 2>/dev/null || true) | wc -l

# The gate.
check: fmt-check vet staticcheck errcheck generate-check boundary test bench kb-check ui-test lines
    @echo "check: green"
