package seeds

import (
	"context"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"

	"forge/internal/core/store"
)

// Import creates seeded workflows and routines once, skips names that exist
// (the operator's rows — or archived ghosts — always win), and tolerates a
// missing seeds dir.
func TestImport(t *testing.T) {
	ctx := context.Background()
	home := t.TempDir()
	st, err := store.Open(ctx, filepath.Join(home, "forge.sqlite3"), store.Options{Clock: func() time.Time { return time.Date(2026, 9, 4, 12, 0, 0, 0, time.UTC) }})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	}()
	lib := filepath.Join(home, "directives")
	for path, content := range map[string]string{
		"seeds/workflows/flow.json": `{"name": "seeded-flow", "graph": {"nodes": [{"id": "only", "type": "directive", "config": {"directive": "triage-repo"}, "position": {"x": 0, "y": 0}}], "edges": []}}`,
		"seeds/routines/cron.json":  `{"name": "seeded-cron", "target": "directive:triage-repo", "objective": "obj", "schedule": "0 7 * * *", "schedule_enabled": false}`,
		"seeds/routines/wf.json":    `{"name": "seeded-wf-trigger", "target": "workflow:seeded-flow"}`,
		"seeds/routines/taken.json": `{"name": "taken", "target": "directive:triage-repo"}`,
		"seeds/routines/bad.json":   `{"name": "Bad Name!", "target": "directive:triage-repo"}`,
	} {
		full := filepath.Join(lib, path)
		if err := os.MkdirAll(filepath.Dir(full), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(full, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	// The operator already owns "taken".
	if err := st.Write(ctx, func(tx *store.Tx) error {
		return tx.CreateRoutine(ctx, &store.Routine{Name: "taken", Target: "directive:mine", Objective: "mine", TimeoutSeconds: 300})
	}); err != nil {
		t.Fatal(err)
	}

	added, err := Import(ctx, st, lib, slog.Default())
	if err != nil {
		t.Fatal(err)
	}
	want := map[string]bool{"workflow:seeded-flow": true, "routine:seeded-cron": true, "routine:seeded-wf-trigger": true}
	if len(added) != len(want) {
		t.Fatalf("added = %v", added)
	}
	for _, a := range added {
		if !want[a] {
			t.Errorf("unexpected addition %q", a)
		}
	}
	rt, err := st.GetRoutine(ctx, "seeded-cron")
	if err != nil || rt.Target != "directive:triage-repo" || rt.ScheduleEnabled {
		t.Errorf("seeded-cron = %+v, %v", rt, err)
	}
	if taken, err := st.GetRoutine(ctx, "taken"); err != nil || taken.Objective != "mine" {
		t.Errorf("operator row touched: %+v, %v", taken, err)
	}
	if _, err := st.GetWorkflow(ctx, "seeded-flow"); err != nil {
		t.Errorf("seeded workflow missing: %v", err)
	}

	// Second import: nothing new.
	again, err := Import(ctx, st, lib, slog.Default())
	if err != nil || len(again) != 0 {
		t.Errorf("second import = %v, %v", again, err)
	}
	// No seeds dir at all: no-op.
	if none, err := Import(ctx, st, t.TempDir(), slog.Default()); err != nil || len(none) != 0 {
		t.Errorf("missing dir = %v, %v", none, err)
	}
}

// Seed workflows carry description/tool metadata onto the row.
func TestImportWorkflowMetadata(t *testing.T) {
	ctx := context.Background()
	home := t.TempDir()
	st, err := store.Open(ctx, filepath.Join(home, "forge.sqlite3"), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	}()
	lib := filepath.Join(home, "directives")
	path := filepath.Join(lib, "seeds", "workflows", "meta.json")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	seed := `{"name": "meta-flow", "description": "seeded and callable", "tool": true, "graph": {"nodes": [{"id": "only", "type": "directive", "config": {"directive": "triage-repo"}, "position": {"x": 0, "y": 0}}], "edges": []}}`
	if err := os.WriteFile(path, []byte(seed), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := Import(ctx, st, lib, slog.Default()); err != nil {
		t.Fatal(err)
	}
	wf, err := st.GetWorkflow(ctx, "meta-flow")
	if err != nil || wf.Description != "seeded and callable" || !wf.Tool {
		t.Fatalf("seeded workflow = %+v, %v", wf, err)
	}
}
