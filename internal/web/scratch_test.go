package web

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/store"
)

// scratchCall invokes forge_scratch as an attempt.
func (h *harness) scratchCall(attemptID string, input map[string]any) (int, []byte) {
	h.t.Helper()
	return h.bridgeTool(attemptID, "forge_scratch", input)
}

// The organic lifecycle: an agent saves-and-runs a scratch script, reuse by
// name counts, it shows up in search, and at the threshold it promotes
// itself into the git library — tool-flagged when it carried a schema.
func TestScratchLifecycle(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.srv.scratchCfg = config.ScratchConfig{Max: 200, PromoteRuns: 3, PromoteAttempts: 1}
	h.createRoutine("caller")
	h.run("caller")
	claim := h.mustClaim("scr-1")

	// Save and run.
	status, body := h.scratchCall(claim.AttemptID, map[string]any{
		"name": "csv-cols", "language": "js",
		"source":       "function main(input) { return {cols: input.params.header.split(',').length} }",
		"description":  "count the columns of a csv header line",
		"input_schema": map[string]any{"type": "object"},
		"input":        map[string]any{"header": "a,b,c"},
	})
	if status != http.StatusOK || !strings.Contains(string(body), `"cols":3`) || !strings.Contains(string(body), `"run_count":1`) {
		t.Fatalf("first run = %d %s", status, body)
	}

	// Re-run by name only.
	status, body = h.scratchCall(claim.AttemptID, map[string]any{"name": "csv-cols", "input": map[string]any{"header": "a,b"}})
	if status != http.StatusOK || !strings.Contains(string(body), `"cols":2`) || !strings.Contains(string(body), `"run_count":2`) {
		t.Fatalf("second run = %d %s", status, body)
	}

	// Search finds it as kind scratch.
	status, body = h.bridgeTool(claim.AttemptID, "forge_library", map[string]any{"query": "csv columns", "kind": "scratch"})
	if status != http.StatusOK || !strings.Contains(string(body), `"kind":"scratch"`) || !strings.Contains(string(body), "csv-cols") {
		t.Fatalf("search = %d %s", status, body)
	}

	// Third run crosses the threshold: automatic promotion.
	status, body = h.scratchCall(claim.AttemptID, map[string]any{"name": "csv-cols", "input": map[string]any{"header": "a"}})
	if status != http.StatusOK || !strings.Contains(string(body), `"promoted":true`) {
		t.Fatalf("third run = %d %s", status, body)
	}
	raw, err := os.ReadFile(filepath.Join(h.libDir, "scripts", "csv-cols.js"))
	if err != nil {
		t.Fatalf("promoted file: %v", err)
	}
	for _, want := range []string{"/**forge", "description: count the columns", "tool: true", "function main"} {
		if !strings.Contains(string(raw), want) {
			t.Errorf("promoted file missing %q:\n%s", want, raw)
		}
	}
	// The library sees it (hot-reloaded) and the cache row is gone.
	if f := h.srv.promptLibrary().Script("csv-cols"); f == nil || !f.Tool {
		t.Fatalf("library script = %+v", f)
	}
	if rows, err := h.st.ListScratch(context.Background()); err != nil || len(rows) != 0 {
		t.Errorf("scratch rows after promotion = %v, %v", rows, err)
	}
	// And it now runs through the ordinary script tool.
	status, body = h.bridgeTool(claim.AttemptID, "forge_script_run", map[string]any{"script": "csv-cols", "input": map[string]any{"header": "x,y"}})
	if status != http.StatusOK || !strings.Contains(string(body), `"cols":2`) {
		t.Fatalf("promoted run = %d %s", status, body)
	}
}

// A subprocess scratch script (any language, the operator's explicit choice)
// runs from the cache dir; one WITHOUT a schema promotes non-tool-flagged.
func TestScratchSubprocessAndNonToolPromotion(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.srv.scratchCfg = config.ScratchConfig{Max: 200, PromoteRuns: 2, PromoteAttempts: 1}
	h.createRoutine("caller")
	h.run("caller")
	claim := h.mustClaim("scr-sub")

	status, body := h.scratchCall(claim.AttemptID, map[string]any{
		"name": "liner", "language": "sh",
		"source":      `read line; echo "{\"len\": ${#line}}"`,
		"description": "length of the stdin payload",
		"input":       map[string]any{"x": 1},
	})
	if status != http.StatusOK || !strings.Contains(string(body), `"len":`) {
		t.Fatalf("sh run = %d %s", status, body)
	}
	status, body = h.scratchCall(claim.AttemptID, map[string]any{"name": "liner"})
	if status != http.StatusOK || !strings.Contains(string(body), `"promoted":true`) {
		t.Fatalf("promotion run = %d %s", status, body)
	}
	raw, err := os.ReadFile(filepath.Join(h.libDir, "scripts", "liner.sh"))
	if err != nil || !strings.Contains(string(raw), "#forge") || strings.Contains(string(raw), "tool: true") {
		t.Fatalf("promoted sh = %v\n%s", err, raw)
	}
	if f := h.srv.promptLibrary().Script("liner"); f == nil || f.Tool || len(f.Interpreter) == 0 {
		t.Fatalf("library sh script = %+v", f)
	}
}

// Refusals and the LRU bound.
func TestScratchRefusalsAndLRU(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.srv.scratchCfg = config.ScratchConfig{Max: 3, PromoteRuns: 99, PromoteAttempts: 99}
	h.withPrompts(map[string]string{"scripts/taken.js": "function main(i){return 1}"})
	h.createRoutine("caller")
	h.run("caller")
	claim := h.mustClaim("scr-r")

	for name, input := range map[string]map[string]any{
		"library name": {"name": "taken", "language": "js", "source": "function main(i){return 1}", "description": "d"},
		"bad language": {"name": "x", "language": "zig", "source": "s", "description": "d"},
		"no desc":      {"name": "x", "language": "js", "source": "function main(i){return 1}"},
		"embedded hdr": {"name": "x", "language": "js", "source": "/**forge\n * tool: true\n */\nfunction main(i){return 1}", "description": "d"},
		"js no main":   {"name": "x", "language": "js", "source": "var a = 1", "description": "d"},
		"unknown name": {"name": "never-saved"},
	} {
		if status, _ := h.scratchCall(claim.AttemptID, input); status != http.StatusBadRequest && status != http.StatusNotFound {
			t.Errorf("%s = %d, want 4xx", name, status)
		}
	}

	// LRU: cap 3, four inserts → the least recently used goes.
	for i := 0; i < 4; i++ {
		status, body := h.scratchCall(claim.AttemptID, map[string]any{
			"name": fmt.Sprintf("s-%d", i), "language": "js",
			"source": fmt.Sprintf("function main(i){return %d}", i), "description": "d",
		})
		if status != http.StatusOK {
			t.Fatalf("insert %d = %d %s", i, status, body)
		}
		h.clock.Advance(time.Second) // distinct last_run ordering under the fake clock
	}
	rows, err := h.st.ListScratch(context.Background())
	if err != nil || len(rows) != 3 {
		t.Fatalf("rows = %d, %v", len(rows), err)
	}
	for _, r := range rows {
		if r.Name == "s-0" {
			t.Error("LRU kept the oldest row")
		}
	}

	// Replacing source resets the counters (a different script, same name).
	if status, _ := h.scratchCall(claim.AttemptID, map[string]any{"name": "s-3", "language": "js", "source": "function main(i){return 99}", "description": "d2"}); status != http.StatusOK {
		t.Fatal("replace failed")
	}
	var row *store.ScratchScript
	if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		var err error
		row, err = tx.GetScratch(context.Background(), "s-3")
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if row.RunCount != 1 || row.Description != "d2" {
		t.Errorf("replaced row = %+v", row)
	}
}
