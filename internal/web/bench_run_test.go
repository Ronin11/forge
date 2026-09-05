package web

import (
	"context"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"encoding/json"

	"forge/internal/core/config"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// wireBench points the harness at a temp spec dir with one spec and fakes the
// add-repo seam (no real git clone machinery in tests; InitRepo still runs).
func (h *harness) wireBench(t *testing.T) string {
	t.Helper()
	specs := t.TempDir()
	spec := "---\nsize: M\nmodel: haiku\nautonomy: auto\nmax_turns: 20\ntimeout: 900\n---\nBuild the thing.\n"
	if err := os.WriteFile(filepath.Join(specs, "quick.md"), []byte(spec), 0o644); err != nil {
		t.Fatal(err)
	}
	h.srv.benchCfg = config.BenchConfig{SpecsDir: specs, RepoParent: t.TempDir()}
	h.srv.addRepo = func(ctx context.Context, path, url, name string) (protocol.Repository, error) {
		return protocol.Repository{Name: filepath.Base(path), Path: path, OriginIdentity: "local/" + filepath.Base(path), Project: "default"}, nil
	}
	return specs
}

// A bench-target trigger routine: the scheduler fires a fresh throwaway run,
// skips while the last tree is open, and the manual run path works too.
func TestBenchRoutineFires(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.writeDirective("plan-project", "---\nmode: plan\nmodel: sonnet\n---\nPlan it: {{objective}}\n")
	h.wireBench(t)

	r := store.Routine{Name: "nightly-bench", Target: "bench:quick", Repositories: []string{"equitizr"},
		TimeoutSeconds: 300, Schedule: "0 5 * * *", ScheduleEnabled: true}
	h.call(http.MethodPost, "/api/v1/routines", r, nil, http.StatusCreated)

	// Make it due and tick.
	if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		return tx.SetNextDue(context.Background(), "nightly-bench", h.clock.Now().Add(-time.Minute))
	}); err != nil {
		t.Fatal(err)
	}
	h.srv.fireDueRoutines(context.Background(), h.clock.Now())

	open, err := h.st.OpenWorkCountForSubmitter(context.Background(), "bench:quick")
	if err != nil || open != 1 {
		t.Fatalf("open bench works = %d, %v (want 1)", open, err)
	}
	works, err := h.st.ListWork(context.Background(), 5)
	if err != nil {
		t.Fatal(err)
	}
	root := works[0]
	if root.SubmittedBy != "bench:quick" || root.Title != "bench: quick" || !root.FinishedAt.IsZero() {
		t.Fatalf("bench root = %+v", root)
	}
	var snap store.Routine
	if err := json.Unmarshal(root.Snapshot, &snap); err != nil || snap.MaxTurns != 20 || snap.TimeoutSeconds != 900 {
		t.Fatalf("snapshot = %+v, %v", snap, err)
	}
	if snap.Target != "directive:plan-project" || !strings.Contains(snap.Prompt, "Build the thing.") {
		t.Fatalf("root not keyed through plan-project: target=%s prompt=%q", snap.Target, snap.Prompt)
	}

	// Still open: the next due firing skips instead of stacking a second run.
	if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		return tx.SetNextDue(context.Background(), "nightly-bench", h.clock.Now().Add(-time.Minute))
	}); err != nil {
		t.Fatal(err)
	}
	h.srv.fireDueRoutines(context.Background(), h.clock.Now())
	if open, _ = h.st.OpenWorkCountForSubmitter(context.Background(), "bench:quick"); open != 1 {
		t.Fatalf("open after skip = %d, want still 1", open)
	}

	// A bench routine cannot run as a plain Work.
	if status, body := h.do(http.MethodPost, "/api/v1/tasks", map[string]any{"routine": "nightly-bench"}, nil, testToken); status != http.StatusBadRequest {
		t.Fatalf("bench routine as Work = %d %s", status, body)
	}

	// The direct endpoint (a different minute so the repo name is fresh).
	h.clock.Advance(2 * time.Minute)
	h.call(http.MethodPost, "/api/v1/bench/quick/run", nil, nil, http.StatusCreated)
	if open, _ = h.st.OpenWorkCountForSubmitter(context.Background(), "bench:quick"); open != 2 {
		t.Fatalf("open after manual run = %d, want 2", open)
	}
}
