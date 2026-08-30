package worker

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func testManifest() Manifest {
	return Manifest{
		AttemptID: strings.Repeat("a", 32), TargetID: strings.Repeat("b", 32), WorkID: strings.Repeat("c", 32),
		WorkerID: "w", RoutineName: "inventory", Repository: "r", RepositoryPath: "/tmp/r",
		RemoteIdentity: "github.com/o/r", BaseBranch: "main", BaseCommit: strings.Repeat("0", 40),
		WorktreePath: "/tmp/wt", Branch: "forge/inventory-aaaaaaaa", Lifecycle: LifecyclePreparing,
	}
}

func TestManifestRoundTrip(t *testing.T) {
	store, err := NewManifestStore(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	m := testManifest()
	if err := store.Create(m); err != nil {
		t.Fatal(err)
	}
	if err := store.Create(m); err == nil {
		t.Error("duplicate create accepted")
	}
	p, _ := store.path(m.AttemptID)
	info, err := os.Stat(p)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Errorf("manifest mode %o", info.Mode().Perm())
	}
	got, err := store.Load(m.AttemptID)
	if err != nil {
		t.Fatal(err)
	}
	if got.Branch != m.Branch || got.Lifecycle != LifecyclePreparing || got.CreatedAt.IsZero() {
		t.Errorf("round trip mismatch: %+v", got)
	}
	updated, err := store.Update(m.AttemptID, func(m *Manifest) {
		m.Lifecycle = LifecycleRunning
		m.PID = 4242
		m.ProcessIdentity = "12345"
	})
	if err != nil {
		t.Fatal(err)
	}
	if updated.PID != 4242 || !updated.UpdatedAt.After(got.UpdatedAt) && updated.UpdatedAt != got.UpdatedAt {
		t.Errorf("update not applied: %+v", updated)
	}
	if _, err := store.Update(m.AttemptID, func(m *Manifest) { m.PID = 0 }); err == nil {
		t.Error("partial process identity accepted")
	}
	all, err := store.LoadAll()
	if err != nil || len(all) != 1 || all[0].PID != 4242 {
		t.Errorf("LoadAll: %v %+v", err, all)
	}
	// Stale temp files are ignored; garbage is reported but not fatal.
	if err := os.WriteFile(filepath.Join(store.dir, "."+m.AttemptID+".json.123.tmp"), []byte("{"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(store.dir, strings.Repeat("d", 32)+".json"), []byte("{bad"), 0o600); err != nil {
		t.Fatal(err)
	}
	all, err = store.LoadAll()
	if err == nil || len(all) != 1 {
		t.Errorf("LoadAll with garbage: err=%v n=%d", err, len(all))
	}
	if _, err := store.Load("../etc/passwd"); err == nil {
		t.Error("path traversal accepted")
	}
}
