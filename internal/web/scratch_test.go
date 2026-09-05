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
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// scratchCall invokes forge_scratch as an attempt.
func (h *harness) scratchCall(attemptID string, input map[string]any) (int, []byte) {
	h.t.Helper()
	return h.bridgeTool(attemptID, "forge_scratch", input)
}

// wireLibraryRepo registers the harness library dir as a repository (the
// daemon does this at bootstrap) and drops in a promote-scratch directive —
// the two preconditions of queued promotion.
func (h *harness) wireLibraryRepo() {
	h.t.Helper()
	h.writeDirective("promote-scratch", "---\nmode: run\nmodel: haiku\n---\nCurate the scratch script into {{repo}}: {{objective}}\n")
	if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		return tx.UpsertProvisionalRepository(context.Background(), protocol.Repository{Name: "directives", Path: h.libDir, OriginIdentity: "local/directives"})
	}); err != nil {
		h.t.Fatal(err)
	}
}

// promotionWork fetches the curation Work recorded on a scratch row.
func (h *harness) promotionWork(name string) (*store.ScratchScript, *store.Work) {
	h.t.Helper()
	var row *store.ScratchScript
	var w *store.Work
	if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		var err error
		if row, err = tx.GetScratch(context.Background(), name); err != nil {
			return err
		}
		if row.PromoteWork == "" {
			return nil
		}
		w, err = tx.GetWork(context.Background(), row.PromoteWork)
		return err
	}); err != nil {
		h.t.Fatal(err)
	}
	return row, w
}

// The organic lifecycle: an agent saves-and-runs a scratch script, reuse by
// name counts, it shows up in search, and at the threshold a high-priority
// curation Work is queued against the library repository; the reconcile
// sweep retires the row once the script answers from the library.
func TestScratchLifecycle(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.srv.scratchCfg = config.ScratchConfig{Max: 200, PromoteRuns: 3, PromoteAttempts: 1}
	h.wireLibraryRepo()
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

	// Third run crosses the threshold: a curation Work is queued, high
	// priority, cause=promotion, integrate-on-green, against the library repo.
	status, body = h.scratchCall(claim.AttemptID, map[string]any{"name": "csv-cols", "input": map[string]any{"header": "a"}})
	if status != http.StatusOK || !strings.Contains(string(body), `"promotion_queued"`) {
		t.Fatalf("third run = %d %s", status, body)
	}
	row, w := h.promotionWork("csv-cols")
	if w == nil {
		t.Fatalf("no promotion work on row %+v", row)
	}
	if w.RoutineName != "promote-scratch" || w.Cause != model.CausePromotion || !w.Integrate ||
		w.Priority != promotionPriority || w.SubmittedBy != "forge:scratch" || w.CausedByWorkID == "" {
		t.Fatalf("promotion work = %+v", w)
	}
	if !strings.Contains(string(w.Snapshot), `"directives"`) || !strings.Contains(string(w.Snapshot), "csv-cols") {
		t.Fatalf("promotion snapshot = %s", w.Snapshot)
	}

	// A fourth run past the threshold does NOT queue a second Work.
	if status, _ := h.scratchCall(claim.AttemptID, map[string]any{"name": "csv-cols", "input": map[string]any{"header": "a"}}); status != http.StatusOK {
		t.Fatal("fourth run failed")
	}
	if again, w2 := h.promotionWork("csv-cols"); w2 == nil || w2.ID != w.ID || again.PromoteWork != w.ID {
		t.Fatalf("re-queued: %+v", w2)
	}

	// The curator lands the script in the library (simulated); the reconcile
	// sweep retires the cache row and the script answers as a real tool.
	path := filepath.Join(h.libDir, "scripts", "csv-cols.js")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	src := "/**forge\n * description: count the columns of a csv header line\n * input: {\"type\":\"object\"}\n * tool: true\n */\nfunction main(input) { return {cols: input.params.header.split(',').length} }\n"
	if err := os.WriteFile(path, []byte(src), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := h.srv.promptsReload(); err != nil {
		t.Fatal(err)
	}
	h.srv.reconcileScratch(context.Background())
	if rows, err := h.st.ListScratch(context.Background()); err != nil || len(rows) != 0 {
		t.Errorf("scratch rows after reconcile = %v, %v", rows, err)
	}
	status, body = h.bridgeTool(claim.AttemptID, "forge_script_run", map[string]any{"script": "csv-cols", "input": map[string]any{"header": "x,y"}})
	if status != http.StatusOK || !strings.Contains(string(body), `"cols":2`) {
		t.Fatalf("promoted run = %d %s", status, body)
	}
}

// A subprocess scratch script (any language, the operator's explicit choice)
// runs from the cache dir; a dead curation Work is cleared by the reconcile
// sweep so a later run re-queues promotion.
func TestScratchSubprocessAndPromotionRetry(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.srv.scratchCfg = config.ScratchConfig{Max: 200, PromoteRuns: 2, PromoteAttempts: 1}
	h.wireLibraryRepo()
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
	if status != http.StatusOK || !strings.Contains(string(body), `"promotion_queued"`) {
		t.Fatalf("promotion run = %d %s", status, body)
	}
	_, w := h.promotionWork("liner")
	if w == nil {
		t.Fatal("no promotion work")
	}

	// The curation Work dies (cancelled): the sweep clears promote_work and
	// the next run past the threshold queues a fresh one.
	h.call(http.MethodDelete, "/api/v1/work/"+w.ID, nil, nil, http.StatusOK)
	h.srv.reconcileScratch(context.Background())
	row, _ := h.promotionWork("liner")
	if row.PromoteWork != "" {
		t.Fatalf("promote_work not cleared: %+v", row)
	}
	if status, body := h.scratchCall(claim.AttemptID, map[string]any{"name": "liner"}); status != http.StatusOK || !strings.Contains(string(body), `"promotion_queued"`) {
		t.Fatalf("re-queue run = %d %s", status, body)
	}
	if _, w2 := h.promotionWork("liner"); w2 == nil || w2.ID == w.ID {
		t.Fatalf("expected a fresh promotion work, got %+v", w2)
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
