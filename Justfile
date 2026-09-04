# Forge developer tasks. `just check` is the gate at every milestone boundary.

set shell := ["bash", "-euo", "pipefail", "-c"]

staticcheck_version := "v0.8.1"
errcheck_version := "v1.20.0"

# Build the binary into ./forge with the git describe as its version. The
# base library (internal/core/directives/starter) is a submodule; a fresh
# clone needs it before go:embed has anything to embed.
submodules:
    test -f internal/core/directives/starter/UPSTREAM || git submodule update --init

build: submodules
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

# End-to-end integration tests: a real daemon, the worker child it spawns, and
# the store, driven over the HTTP-on-unix-socket API with the fake-claude
# executor (internal/core/eval/integration_test.go). Hermetic — a temporary
# FORGE_HOME per case, no network, no `claude`, no budget. Behind the
# `integration` build tag so `just check` keeps its runtime; run this whenever
# the daemon/worker/store contract or the lifecycle states change.
test-integration:
    go test -tags integration -race -count=1 -run 'TestIntegration' ./internal/core/eval/...

# Benchmarks on the event path and the stats query with a regression threshold
# recorded in bench/threshold.txt (added in M1 with the first benchmark).
bench:
    @if [ -f bench/threshold.txt ]; then ./scripts/bench-check.sh; else echo "bench: no benchmarks yet (M1)"; fi

# Layering, proved by go list (MODULARIZATION.md §7). Fails closed: a rule whose
# path matches no packages is itself a failure, so a `git mv` cannot silently
# evaporate a rule. The tree table mirrors §3 on today's layout — internal/web
# holds `web` (the ceiling: may reach anything), every other internal tree is
# `core` (may not reach web) — plus the three finer rules that predate
# it: worker never imports web or store (one SQLite writer); model and
# protocol import nothing of Forge (protocol may see model). grep is not run
# with -q so a SIGPIPE cannot turn a violation into a pass under pipefail.
#
#   tree   packages                                 may reach
#   core   internal/* except web, tools, tui        core
#   tools  internal/tools                           core tools
#   web    internal/web                             core tools web
#   tui    internal/tui                             core tui
#
# §6.3 exception: tui imports core/store for its row TYPES only (DTOs decoded
# from API responses); promoting them into protocol is its own later task.
# go list cannot see "types only", so the store edge is simply legal core —
# this comment is the exception's single recorded home.
boundary:
    @check() { desc="$1"; list="$2"; deny="$3"; allow="$4"; \
        if [ -z "$list" ]; then echo "boundary: $desc: rule matches no packages (fail closed)"; exit 1; fi; \
        for p in $list; do \
            bad=$(go list -deps "$p" | grep -E "$deny" | grep -vx "$p" | grep -Ev "$allow" || true); \
            if [ -n "$bad" ]; then echo "boundary: $desc: $p imports:"; echo "$bad"; exit 1; fi; \
        done; }; \
    check "core may not reach web, tools, or tui" \
        "$(go list ./internal/... 2>/dev/null | grep -vE '^forge/internal/(web|tools|tui)' || true)" \
        '^forge/internal/(web|tools)(/|$)' '^$'; \
    check "tui may reach core and tui only" \
        "$(go list ./internal/tui/... 2>/dev/null || true)" \
        '^forge/internal/(web|tools)(/|$)' '^$'; \
    check "tools may reach core and tools only" \
        "$(go list ./internal/tools/... 2>/dev/null || true)" \
        '^forge/internal/web(/|$)' '^$'; \
    check "web tree present" \
        "$(go list ./internal/web/... 2>/dev/null || true)" \
        '^$' '^$'; \
    check "worker may not reach web or store" \
        "$(go list ./internal/core/worker/... 2>/dev/null || true)" \
        '^forge/internal/(web|core/store)(/|$)' '^$'; \
    check "model imports nothing of forge" \
        "$(go list ./internal/core/model/... 2>/dev/null || true)" \
        '^forge/' '^$'; \
    check "protocol imports nothing of forge but model" \
        "$(go list ./internal/core/protocol/... 2>/dev/null || true)" \
        '^forge/' '^forge/internal/core/model$'; \
    engfiles=$(grep -l '^func (s \*Engine)' internal/web/*.go 2>/dev/null || true); \
    if [ -z "$engfiles" ]; then echo "boundary: no Engine method files found (fail closed)"; exit 1; fi; \
    bad=$(echo "$engfiles" | xargs grep -l '"net/http"' 2>/dev/null || true); \
    if [ -n "$bad" ]; then echo "boundary: Engine methods defined in files importing net/http:"; echo "$bad"; exit 1; fi; \
    echo "boundary: ok"

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
check: fmt-check vet staticcheck errcheck generate-check boundary test bench kb-check ui-test lines eval-check
    @echo "check: green"

# Build first-party plugin binaries (M7). The installer runs each manifest's
# build argv; this is the developer convenience for the same step.
build-plugins:
    cd plugins/status-file && go build -o forge-status-file .
    cd plugins/notify && go build -o forge-notify .
    cd plugins/github-issues && go build -o forge-github-issues .
    cd plugins/signal && go build -o forge-signal .
    cd plugins/teams && go build -o forge-teams .
    cd plugins/email && go build -o forge-email .

# M12: golden eval cases through the fake executor (offline, no budget).
eval-check: build
    ./scripts/eval-check.sh
