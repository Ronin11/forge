set shell := ["bash", "-euo", "pipefail", "-c"]

build:
    go build -o forge ./cmd/forge

run: build
    ./forge run

test:
    go test -race ./...

vet:
    go vet ./...

fmt-check:
    test -z "$(gofmt -l cmd internal)" || (gofmt -l cmd internal; exit 1)

staticcheck:
    go run honnef.co/go/tools/cmd/staticcheck@latest ./...

# The worker must never import the control plane.
boundary:
    ! go list -deps ./internal/worker | grep -q 'forge/internal/controlplane'
    ! go list -deps ./internal/model ./internal/protocol | grep -qE 'forge/internal/(worker|controlplane)'

lines:
    @echo "Go (non-test):"; find . -name '*.go' ! -name '*_test.go' -not -path './.scratch/*' | xargs wc -l | tail -1
    @echo "Go (test):"; find . -name '*_test.go' -not -path './.scratch/*' | xargs wc -l | tail -1
    @echo "UI:"; find internal/controlplane/ui -type f | xargs wc -l | tail -1

check: fmt-check vet boundary test staticcheck lines
