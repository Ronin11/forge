package migratedirectives

import (
	"context"
	"log/slog"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"forge/internal/core/directives"
	"forge/internal/core/store"
)

func bg() context.Context { return context.Background() }

type fixture struct {
	t      *testing.T
	st     *store.Store
	home   string
	libDir string
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	home := t.TempDir()
	st, err := store.Open(bg(), filepath.Join(home, "forge.sqlite3"), store.Options{Clock: func() time.Time { return time.Date(2026, 9, 4, 12, 0, 0, 0, time.UTC) }})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})
	return &fixture{t: t, st: st, home: home, libDir: filepath.Join(home, "directives")}
}

func (f *fixture) write(fn func(tx *store.Tx) error) {
	f.t.Helper()
	if err := f.st.Write(bg(), fn); err != nil {
		f.t.Fatal(err)
	}
}

func (f *fixture) run(dry bool) Report {
	f.t.Helper()
	rep, err := Run(bg(), f.st, f.home, f.libDir, dry, slog.Default())
	if err != nil {
		f.t.Fatal(err)
	}
	return rep
}

func (f *fixture) seedOldLibrary(files map[string]string) {
	f.t.Helper()
	old := filepath.Join(f.home, "prompts")
	for name, content := range files {
		path := filepath.Join(old, name)
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			f.t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			f.t.Fatal(err)
		}
	}
}

// The full pass: dir renames (git history rides along), every content
// routine splits into a byte-deterministic directive file with the row
// re-pointed and a generation snapshot of the pre-split state, graphs
// convert with the envelope baked, and a second run is a complete no-op.
func TestRunFullAndIdempotent(t *testing.T) {
	f := newFixture(t)
	f.seedOldLibrary(map[string]string{"personas/triager.md": "---\nmodel: haiku\n---\nYou triage."})
	f.write(func(tx *store.Tx) error {
		if err := tx.CreateRoutine(bg(), &store.Routine{Name: "sweep", Mode: "run", Prompt: "Sweep {{repo}}: {{objective}}\n", Persona: "triager", Effort: "low", Repositories: []string{"equitizr"}, TimeoutSeconds: 900, MaxTurns: 12, BudgetClass: "backlog"}); err != nil {
			return err
		}
		return tx.CreateRoutine(bg(), &store.Routine{Name: "fix", Mode: "run", Prompt: "Fix it.", Model: "sonnet", TimeoutSeconds: 300})
	})
	f.write(func(tx *store.Tx) error {
		return tx.CreateWorkflow(bg(), &store.Workflow{Name: "nightly", Graph: &store.WorkflowGraph{
			Nodes: []store.WorkflowNode{
				{ID: "a", Type: store.NodeRoutine, Config: map[string]any{"routine": "sweep", "objective": "from {{run.objective}}"}},
				{ID: "b", Type: store.NodeRoutine, Config: map[string]any{"routine": "fix", "repositories": []string{"equitizr"}}},
			},
			Edges: []store.WorkflowGraphEdge{{From: "a", To: "b", When: store.WhenSuccess}},
		}})
	})

	rep := f.run(false)
	if !rep.DirRenamed || len(rep.RoutinesSplit) != 2 || len(rep.GraphsConverted) != 1 || rep.Skipped != nil {
		t.Fatalf("report = %+v", rep)
	}
	// The old dir is gone; the persona moved with it.
	if _, err := os.Stat(filepath.Join(f.home, "prompts")); !os.IsNotExist(err) {
		t.Error("old prompts dir still exists")
	}
	raw, err := os.ReadFile(filepath.Join(f.libDir, "directives", "sweep.md"))
	if err != nil {
		t.Fatal(err)
	}
	want := "---\nmode: run\npersona: triager\neffort: low\n---\nSweep {{repo}}: {{objective}}\n"
	if string(raw) != want {
		t.Errorf("sweep.md = %q, want %q", raw, want)
	}
	lib, err := directives.Load(f.libDir)
	if err != nil {
		t.Fatal(err)
	}
	if lib.Directive("sweep") == nil || lib.Directive("fix") == nil || lib.Persona("triager") == nil {
		t.Error("library missing migrated content")
	}
	if lib.Dirty {
		t.Error("library left dirty; the split should commit")
	}
	// Rows are trigger shells now; generation history keeps the content.
	rt, err := f.st.GetRoutine(bg(), "sweep")
	if err != nil {
		t.Fatal(err)
	}
	if rt.Target != "directive:sweep" || rt.Prompt != "" || rt.Persona != "" || rt.Generation != 2 || rt.TimeoutSeconds != 900 {
		t.Errorf("sweep row = %+v", rt)
	}
	f.write(func(tx *store.Tx) error {
		snap, err := tx.GenerationSnapshot(bg(), rt.ID, 1)
		if err != nil {
			return err
		}
		if !strings.Contains(string(snap), "Sweep {{repo}}") {
			t.Errorf("generation 1 lost the content: %s", snap)
		}
		return nil
	})
	// The graph converted with the envelope baked; node bindings survive.
	wf, err := f.st.GetWorkflow(bg(), "nightly")
	if err != nil {
		t.Fatal(err)
	}
	a := wf.Graph.Node("a")
	cfg, err := a.DirectiveConfig()
	if err != nil || a.Type != store.NodeDirective {
		t.Fatalf("node a = %+v, %v", a, err)
	}
	if cfg.Directive != "sweep" || cfg.Objective != "from {{run.objective}}" || cfg.TimeoutSeconds != 900 || cfg.MaxTurns != 12 || cfg.BudgetClass != "backlog" || cfg.Model != "" {
		t.Errorf("node a config = %+v", cfg)
	}

	// Second run: pure no-op.
	if rep2 := f.run(false); !rep2.Empty() {
		t.Errorf("second run = %+v, want empty", rep2)
	}
}

// Dry run reports everything and writes nothing.
func TestRunDry(t *testing.T) {
	f := newFixture(t)
	f.seedOldLibrary(map[string]string{"fragments/x.md": "x"})
	f.write(func(tx *store.Tx) error {
		return tx.CreateRoutine(bg(), &store.Routine{Name: "sweep", Mode: "run", Prompt: "p", Model: "haiku", TimeoutSeconds: 300})
	})
	rep := f.run(true)
	if !rep.DirRenamed || len(rep.RoutinesSplit) != 1 {
		t.Fatalf("dry report = %+v", rep)
	}
	if _, err := os.Stat(f.libDir); !os.IsNotExist(err) {
		t.Error("dry run renamed the dir")
	}
	rt, err := f.st.GetRoutine(bg(), "sweep")
	if err != nil || rt.Target != "" {
		t.Errorf("dry run touched the row: %+v, %v", rt, err)
	}
	// Wet after dry equals dry's promise.
	wet := f.run(false)
	if !reflect.DeepEqual(rep.RoutinesSplit, wet.RoutinesSplit) || wet.DirRenamed != rep.DirRenamed {
		t.Errorf("wet = %+v, dry promised %+v", wet, rep)
	}
}

// Crash between the file write and the row update: the next run overwrites
// the identical file and completes the row.
func TestRunResumesAfterCrash(t *testing.T) {
	f := newFixture(t)
	f.write(func(tx *store.Tx) error {
		return tx.CreateRoutine(bg(), &store.Routine{Name: "sweep", Mode: "run", Prompt: "p", Model: "haiku", TimeoutSeconds: 300})
	})
	// Simulate the half-run: file written, row untouched.
	if err := os.MkdirAll(filepath.Join(f.libDir, "directives"), 0o755); err != nil {
		t.Fatal(err)
	}
	rt, err := f.st.GetRoutine(bg(), "sweep")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(f.libDir, "directives", "sweep.md"), []byte(directiveFile(rt)), 0o644); err != nil {
		t.Fatal(err)
	}
	rep := f.run(false)
	if len(rep.RoutinesSplit) != 1 {
		t.Fatalf("resume report = %+v", rep)
	}
	after, err := f.st.GetRoutine(bg(), "sweep")
	if err != nil || after.Target != "directive:sweep" {
		t.Errorf("row not completed: %+v, %v", after, err)
	}
}

// A hand-authored directive with different content owns the name; the
// routine stays legacy and keeps running.
func TestRunCollisionSkips(t *testing.T) {
	f := newFixture(t)
	f.write(func(tx *store.Tx) error {
		return tx.CreateRoutine(bg(), &store.Routine{Name: "sweep", Mode: "run", Prompt: "p", Model: "haiku", TimeoutSeconds: 300})
	})
	if err := os.MkdirAll(filepath.Join(f.libDir, "directives"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(f.libDir, "directives", "sweep.md"), []byte("---\nmode: run\n---\nhand-authored\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	rep := f.run(false)
	if len(rep.RoutinesSplit) != 0 || rep.Skipped["sweep"] == "" {
		t.Fatalf("collision report = %+v", rep)
	}
	rt, err := f.st.GetRoutine(bg(), "sweep")
	if err != nil || rt.Target != "" || rt.Prompt != "p" {
		t.Errorf("collided routine changed: %+v, %v", rt, err)
	}
	raw, err := os.ReadFile(filepath.Join(f.libDir, "directives", "sweep.md"))
	if err != nil || !strings.Contains(string(raw), "hand-authored") {
		t.Errorf("hand-authored file changed: %q, %v", raw, err)
	}
}

// A custom library path is used as-is: no rename, split lands inside it.
func TestRunCustomPath(t *testing.T) {
	f := newFixture(t)
	f.libDir = filepath.Join(f.home, "my-library")
	f.seedOldLibrary(map[string]string{"personas/p.md": "hi"})
	f.write(func(tx *store.Tx) error {
		return tx.CreateRoutine(bg(), &store.Routine{Name: "sweep", Mode: "run", Prompt: "p", Model: "haiku", TimeoutSeconds: 300})
	})
	rep := f.run(false)
	if rep.DirRenamed {
		t.Error("custom path renamed")
	}
	if _, err := os.Stat(filepath.Join(f.home, "prompts")); err != nil {
		t.Error("old dir touched despite custom path")
	}
	if _, err := os.Stat(filepath.Join(f.libDir, "directives", "sweep.md")); err != nil {
		t.Errorf("split missing under custom path: %v", err)
	}
}

// Both directories present: prefer directives, warn, touch nothing.
func TestRunBothDirsExist(t *testing.T) {
	f := newFixture(t)
	f.seedOldLibrary(map[string]string{"personas/old.md": "old"})
	if err := os.MkdirAll(filepath.Join(f.libDir, "personas"), 0o755); err != nil {
		t.Fatal(err)
	}
	rep := f.run(false)
	if rep.DirRenamed || rep.Skipped["dir:"+filepath.Join(f.home, "prompts")] == "" {
		t.Fatalf("both-dirs report = %+v", rep)
	}
	if _, err := os.Stat(filepath.Join(f.home, "prompts", "personas", "old.md")); err != nil {
		t.Error("old dir was touched")
	}
}

// A prompt with literal {{> include syntax would change meaning inside a
// directive file — it stays legacy.
func TestRunSkipsIncludeSyntax(t *testing.T) {
	f := newFixture(t)
	f.write(func(tx *store.Tx) error {
		return tx.CreateRoutine(bg(), &store.Routine{Name: "inc", Mode: "run", Prompt: "literal {{> thing}}", Model: "haiku", TimeoutSeconds: 300})
	})
	rep := f.run(false)
	if len(rep.RoutinesSplit) != 0 || !strings.Contains(rep.Skipped["inc"], "include syntax") {
		t.Fatalf("report = %+v", rep)
	}
}
