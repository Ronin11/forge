package store

import (
	"context"
	"database/sql"
	"net/url"
	"path/filepath"
	"testing"

	"forge/internal/core/model"
)

// TestMigrationBackfillProvenance drives the real backfill: a database is
// migrated up to just before the provenance migration and seeded with legacy
// rows (a plan batch and a verify follow-up whose only subject link is the
// snapshot's verify_of), then the provenance migration is applied and the
// caused_by / root / cause columns are asserted.
func TestMigrationBackfillProvenance(t *testing.T) {
	migrations, err := loadMigrations()
	if err != nil {
		t.Fatal(err)
	}
	// Locate the provenance migration by id (later migrations may sort after
	// it); the backfill is driven by applying everything strictly before it,
	// then it.
	idx := -1
	for i, m := range migrations {
		if m.id == "01M1CJ8P0V6KQY3W2E9R7T4B5N_work_provenance" {
			idx = i
			break
		}
	}
	if idx < 0 {
		t.Fatal("provenance migration not found")
	}
	last := migrations[idx]
	path := filepath.Join(t.TempDir(), "forge.sqlite3")
	dsn := "file:" + path + "?" + url.Values{"_pragma": {"foreign_keys(1)"}}.Encode()
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	}()
	db.SetMaxOpenConns(1)
	ctx := context.Background()
	// Everything before provenance.
	for _, m := range migrations[:idx] {
		if _, err := db.ExecContext(ctx, m.sql); err != nil {
			t.Fatalf("apply %s: %v", m.id, err)
		}
	}
	// Legacy rows, provenance columns left NULL as they were before this Forge.
	work := func(id, batch, snapshot string) {
		t.Helper()
		if _, err := db.ExecContext(ctx, `INSERT INTO work (id, routine_name, generation, title, trigger, snapshot, priority, budget_class, autonomy, integrate, plan_batch_id, created_at) VALUES (?, 'r', 0, 't', 'manual', ?, 100, 'normal', 'auto', 0, ?, '2026-08-30T12:00:00Z')`,
			id, snapshot, sql.NullString{String: batch, Valid: batch != ""}); err != nil {
			t.Fatalf("insert work %s: %v", id, err)
		}
	}
	work("planwork00000000000000000000000a", "", `{}`)                                 // plan root
	work("planchild0000000000000000000000b", "planwork00000000000000000000000a", `{}`) // plan child
	work("subject000000000000000000000000c", "", `{}`)                                 // verify subject
	// The verify subject's target and attempt: the follow-up links to the
	// attempt id, which the backfill joins back to the subject Work.
	if _, err := db.ExecContext(ctx, `INSERT INTO targets (id, work_id, repository_name, state, created_at, updated_at) VALUES ('target0000000000000000000000000d','subject000000000000000000000000c','demo','succeeded','2026-08-30T12:00:00Z','2026-08-30T12:00:00Z')`); err != nil {
		t.Fatal(err)
	}
	if _, err := db.ExecContext(ctx, `INSERT INTO attempts (id, target_id, worker_id, claim_request_id, mcp_token_hash, executor, model, model_alias, mode, autonomy, launches, created_at, updated_at) VALUES ('attempt000000000000000000000000e','target0000000000000000000000000d','w','cr','h','claude-code','m','haiku','run','auto',1,'2026-08-30T12:00:00Z','2026-08-30T12:00:00Z')`); err != nil {
		t.Fatal(err)
	}
	work("verifywork0000000000000000000f00", "", `{"verify_of":{"attempt_id":"attempt000000000000000000000000e"}}`)

	if _, err := db.ExecContext(ctx, last.sql); err != nil {
		t.Fatalf("apply provenance migration: %v", err)
	}

	type prov struct {
		caused, root, cause sql.NullString
	}
	get := func(id string) prov {
		t.Helper()
		var p prov
		if err := db.QueryRowContext(ctx, `SELECT caused_by_work_id, root_work_id, cause FROM work WHERE id = ?`, id).Scan(&p.caused, &p.root, &p.cause); err != nil {
			t.Fatalf("read %s: %v", id, err)
		}
		return p
	}
	cases := []struct {
		id, caused, root, cause string
	}{
		{"planwork00000000000000000000000a", "", "planwork00000000000000000000000a", ""},
		{"planchild0000000000000000000000b", "planwork00000000000000000000000a", "planwork00000000000000000000000a", "plan_task"},
		{"subject000000000000000000000000c", "", "subject000000000000000000000000c", ""},
		{"verifywork0000000000000000000f00", "subject000000000000000000000000c", "subject000000000000000000000000c", "verify"},
	}
	for _, c := range cases {
		p := get(c.id)
		if p.caused.String != c.caused {
			t.Errorf("%s caused_by = %q, want %q", c.id, p.caused.String, c.caused)
		}
		if p.root.String != c.root {
			t.Errorf("%s root = %q, want %q", c.id, p.root.String, c.root)
		}
		if p.cause.String != c.cause {
			t.Errorf("%s cause = %q, want %q", c.id, p.cause.String, c.cause)
		}
	}
}

// TestCreateWorkRootDerivation covers the store invariant: a root is its own
// root, a child inherits the parent's root, a missing parent is an error, and a
// contradicting root_work_id is rejected.
func TestCreateWorkRootDerivation(t *testing.T) {
	f := newFixture(t)
	mkRoot := func() *Work {
		w := &Work{RoutineName: "r", Title: "root", Trigger: model.TriggerManual, Snapshot: []byte(`{}`), Priority: 1, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto}
		f.write(func(tx *Tx) error {
			_, err := tx.CreateWork(ctx(), w, []string{"equitizr"}, nil)
			return err
		})
		return w
	}
	root := mkRoot()
	if root.RootWorkID != root.ID {
		t.Errorf("root's root = %q, want its own id %q", root.RootWorkID, root.ID)
	}

	child := &Work{RoutineName: "r", Title: "child", Trigger: model.TriggerDependency, Snapshot: []byte(`{}`), Priority: 1, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto, CausedByWorkID: root.ID, Cause: model.CausePlanTask}
	f.write(func(tx *Tx) error {
		_, err := tx.CreateWork(ctx(), child, []string{"equitizr"}, nil)
		return err
	})
	if child.RootWorkID != root.ID {
		t.Errorf("child root = %q, want parent root %q", child.RootWorkID, root.ID)
	}

	grand := &Work{RoutineName: "r", Title: "grandchild", Trigger: model.TriggerDependency, Snapshot: []byte(`{}`), Priority: 1, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto, CausedByWorkID: child.ID, Cause: model.CauseVerify}
	f.write(func(tx *Tx) error {
		_, err := tx.CreateWork(ctx(), grand, []string{"equitizr"}, nil)
		return err
	})
	if grand.RootWorkID != root.ID {
		t.Errorf("grandchild root = %q, want %q", grand.RootWorkID, root.ID)
	}

	// Missing parent is an error.
	orphan := &Work{RoutineName: "r", Title: "orphan", Trigger: model.TriggerDependency, Snapshot: []byte(`{}`), Priority: 1, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto, CausedByWorkID: "ffffffffffffffffffffffffffffffff"}
	err := f.s.Write(ctx(), func(tx *Tx) error {
		_, err := tx.CreateWork(ctx(), orphan, []string{"equitizr"}, nil)
		return err
	})
	if err == nil {
		t.Error("expected error for missing caused_by parent")
	}

	// A root_work_id that contradicts the derivation is rejected.
	bad := &Work{RoutineName: "r", Title: "bad", Trigger: model.TriggerDependency, Snapshot: []byte(`{}`), Priority: 1, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto, CausedByWorkID: root.ID, Cause: model.CausePlanTask, RootWorkID: "00000000000000000000000000000000"}
	err = f.s.Write(ctx(), func(tx *Tx) error {
		_, err := tx.CreateWork(ctx(), bad, []string{"equitizr"}, nil)
		return err
	})
	if err == nil {
		t.Error("expected error for contradicting root_work_id")
	}

	// WorkTree returns the whole tree in created order.
	tree, err := f.s.WorkTree(ctx(), root.ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(tree) != 3 {
		t.Fatalf("tree size = %d, want 3 (root, child, grandchild)", len(tree))
	}
	if tree[0].ID != root.ID || tree[2].ID != grand.ID {
		t.Errorf("tree order = [%s..%s], want root first, grandchild last", tree[0].ID[:8], tree[2].ID[:8])
	}
}
