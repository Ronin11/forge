package main

import (
	"bytes"
	"context"
	"strings"
	"testing"
	"time"
)

// mcpCmdContext is a cmdContext whose environment is empty, so runMCP fails
// before it ever touches stdin.
func mcpCmdContext(t *testing.T) (*cmdContext, *bytes.Buffer) {
	t.Helper()
	var out, errOut bytes.Buffer
	c := &cmdContext{stdout: &out, stderr: &errOut,
		getenv:    func(string) string { return "" },
		forgeHome: t.TempDir(), userHome: t.TempDir(), now: time.Now}
	return c, &errOut
}

func TestRunMCPUsageAndUnreachableDaemon(t *testing.T) {
	ctx := context.Background()

	c, stderr := mcpCmdContext(t)
	if code := runMCP(ctx, c, nil); code != 2 || !strings.Contains(stderr.String(), "--attempt is required") {
		t.Errorf("no --attempt: code %d, stderr %q", code, stderr.String())
	}

	c, stderr = mcpCmdContext(t)
	if code := runMCP(ctx, c, []string{"--attempt", "not-an-id"}); code != 2 || !strings.Contains(stderr.String(), "invalid id") {
		t.Errorf("bad id: code %d, stderr %q", code, stderr.String())
	}

	// A valid id with no daemon address is one line on stderr and exit 1 —
	// forge mcp never auto-starts anything.
	c, stderr = mcpCmdContext(t)
	if code := runMCP(ctx, c, []string{"--attempt", strings.Repeat("ab", 16)}); code != 1 ||
		!strings.Contains(stderr.String(), "FORGE_SOCKET") {
		t.Errorf("no daemon: code %d, stderr %q", code, stderr.String())
	}
	if got := strings.Count(strings.TrimSpace(stderr.String()), "\n"); got != 0 {
		t.Errorf("want exactly one stderr line, got %d extra newlines: %q", got, stderr.String())
	}
}
