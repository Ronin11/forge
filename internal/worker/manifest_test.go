package worker

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/model"
)

const testWorkerID = "0123456789abcdef0123456789abcdef"

func manifestFixture(t *testing.T) (*ManifestStore, string) {
	t.Helper()
	dataDir := t.TempDir()
	fixed := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	s, err := NewManifestStore(dataDir, testWorkerID, func() time.Time { return fixed })
	if err != nil {
		t.Fatal(err)
	}
	return s, dataDir
}

func goodManifest(dataDir string) *Manifest {
	attempt := model.NewID()
	return &Manifest{
		AttemptID: attempt, TargetID: model.NewID(), WorkID: model.NewID(), RoutineName: "inventory",
		RepositoryName: "equitizr", RepositoryPath: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr",
		WorktreePath: filepath.Join(dataDir, "worktrees", attempt), Branch: model.BranchName("inventory", attempt),
		Kind: "attempt", Lifecycle: ManifestPreparing,
	}
}

func TestManifestWriteLoadRoundTripAndIntentFirst(t *testing.T) {
	s, dataDir := manifestFixture(t)
	m := goodManifest(dataDir)
	if err := s.Write(m); err != nil {
		t.Fatal(err)
	}
	if m.SchemaVersion != ManifestSchemaVersion || m.WorkerID != testWorkerID || m.CreatedAt.IsZero() {
		t.Errorf("Write did not stamp: %+v", m)
	}
	info, err := os.Stat(s.Path(m.AttemptID))
	if err != nil || info.Mode().Perm() != 0o600 {
		t.Errorf("manifest mode: %v %v", info, err)
	}
	created := m.CreatedAt
	m.Lifecycle, m.PID, m.PIDStart, m.ProcessActive = ManifestRunning, 4242, 99, true
	if err := s.Write(m); err != nil {
		t.Fatal(err)
	}
	got, err := s.Load(m.AttemptID)
	if err != nil {
		t.Fatal(err)
	}
	if got.Lifecycle != ManifestRunning || got.PID != 4242 || !got.CreatedAt.Equal(created) || got.Branch != m.Branch {
		t.Errorf("round trip: %+v", got)
	}
	entries, err := os.ReadDir(filepath.Join(dataDir, "attempts"))
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".tmp") {
			t.Errorf("temp file left behind: %s", e.Name())
		}
	}
}

func TestManifestLoadRefusesTampering(t *testing.T) {
	s, dataDir := manifestFixture(t)
	m := goodManifest(dataDir)
	if err := s.Write(m); err != nil {
		t.Fatal(err)
	}
	path := s.Path(m.AttemptID)
	original, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	restore := func() {
		if err := os.WriteFile(path, original, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	try := func(err error) {
		t.Helper()
		if err != nil && !os.IsNotExist(err) {
			t.Fatal(err)
		}
	}
	cases := []struct {
		name   string
		mutate func()
		want   string
	}{
		{"world readable", func() { try(os.Chmod(path, 0o644)) }, "group or other access"},
		{"unknown field", func() {
			try(os.WriteFile(path, []byte(strings.Replace(string(original), `"kind"`, `"bogus":1,"kind"`, 1)), 0o600))
		}, "unknown"},
		{"trailing json", func() { try(os.WriteFile(path, append(append([]byte{}, original...), []byte("{}")...), 0o600)) }, "trailing"},
		{"wrong worker", func() {
			try(os.WriteFile(path, []byte(strings.Replace(string(original), testWorkerID, strings.Repeat("f", 32), 1)), 0o600))
		}, "different worker"},
		{"wrong worktree path", func() {
			try(os.WriteFile(path, []byte(strings.Replace(string(original), "/worktrees/", "/elsewhere/", 1)), 0o600))
		}, "owned path"},
		{"wrong branch", func() {
			try(os.WriteFile(path, []byte(strings.Replace(string(original), "forge/inventory-", "forge/other-", 1)), 0o600))
		}, "derived name"},
		{"partial identity", func() {
			try(os.WriteFile(path, []byte(strings.Replace(string(original), `"process_active"`, `"pid":5,"process_active"`, 1)), 0o600))
		}, "partial"},
		{"foreign schema", func() {
			try(os.WriteFile(path, []byte(strings.Replace(string(original), `"schema_version": 1`, `"schema_version": 9`, 1)), 0o600))
		}, "schema version"},
		{"symlink", func() {
			try(os.Rename(path, path+".real"))
			try(os.Symlink(path+".real", path))
		}, "symlink"},
	}
	for _, c := range cases {
		c.mutate()
		_, err := s.Load(m.AttemptID)
		if err == nil || !strings.Contains(strings.ToLower(err.Error()), strings.ToLower(c.want)) {
			t.Errorf("%s: err = %v, want it to mention %q", c.name, err, c.want)
		}
		try(os.Remove(path))
		try(os.Remove(path + ".real"))
		restore()
	}
	if _, err := s.Load(m.AttemptID); err != nil {
		t.Fatalf("restored manifest should load: %v", err)
	}
}

func TestManifestLoadAllAndRemove(t *testing.T) {
	s, dataDir := manifestFixture(t)
	good := goodManifest(dataDir)
	if err := s.Write(good); err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(dataDir, "attempts")
	if err := os.WriteFile(filepath.Join(dir, model.NewID()+".json"), []byte("{not json"), 0o600); err != nil {
		t.Fatal(err)
	}
	stale := filepath.Join(dir, ".stale-123.tmp")
	if err := os.WriteFile(stale, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	manifests, corrupt, err := s.LoadAll()
	if err != nil {
		t.Fatal(err)
	}
	if len(manifests) != 1 || manifests[0].AttemptID != good.AttemptID || len(corrupt) != 1 {
		t.Errorf("LoadAll = %d manifests, %d corrupt", len(manifests), len(corrupt))
	}
	if _, err := os.Stat(stale); !os.IsNotExist(err) {
		t.Error("stale temp file not removed")
	}
	if err := s.Remove(good.AttemptID); err == nil {
		t.Error("Remove accepted a non-final manifest")
	}
	good.Lifecycle = ManifestCleaned
	if err := s.Write(good); err != nil {
		t.Fatal(err)
	}
	if err := s.Remove(good.AttemptID); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(s.Path(good.AttemptID)); !os.IsNotExist(err) {
		t.Error("manifest not removed")
	}
	if !good.Final() || (&Manifest{Lifecycle: ManifestRetained}).Final() {
		t.Error("Final")
	}
	for _, kind := range []string{"greenfield", "merge"} {
		m := goodManifest(dataDir)
		m.Kind, m.SchemaVersion, m.WorkerID, m.CreatedAt = kind, ManifestSchemaVersion, testWorkerID, time.Now()
		if kind == "greenfield" {
			m.WorktreePath = "/home/x/Projects/new"
		}
		if err := s.Validate(m); err != nil {
			t.Errorf("kind %s: %v", kind, err)
		}
	}
	m := goodManifest(dataDir)
	m.Lifecycle, m.SchemaVersion, m.WorkerID, m.CreatedAt = "bogus", ManifestSchemaVersion, testWorkerID, time.Now()
	if err := s.Validate(m); err == nil {
		t.Error("bad lifecycle accepted")
	}
}

func TestNewManifestStoreRefusesSymlinkedDir(t *testing.T) {
	base := t.TempDir()
	real := filepath.Join(base, "real")
	if err := os.MkdirAll(filepath.Join(real, "attempts"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(filepath.Join(real, "attempts"), filepath.Join(base, "attempts")); err != nil {
		t.Fatal(err)
	}
	if _, err := NewManifestStore(base, testWorkerID, time.Now); err == nil {
		t.Error("symlinked attempts dir accepted")
	}
}
