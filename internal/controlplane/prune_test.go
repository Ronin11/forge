package controlplane

import (
	"context"
	"forge/internal/core/config"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestPrune(t *testing.T) {
	dataDir := t.TempDir()
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	ret := config.RetentionConfig{TranscriptDays: 90, OutputDays: 30, ArtifactDays: 90}
	out := filepath.Join(dataDir, "output")
	if err := os.MkdirAll(out, 0o700); err != nil {
		t.Fatal(err)
	}
	write := func(name string, age int) string {
		p := filepath.Join(out, name)
		if err := os.WriteFile(p, []byte("transcript data"), 0o600); err != nil {
			t.Fatal(err)
		}
		mt := now.AddDate(0, 0, -age)
		if err := os.Chtimes(p, mt, mt); err != nil {
			t.Fatal(err)
		}
		return p
	}
	fresh := write("fresh.log", 1)
	toCompress := write("old.log", 10)
	toDrop := write("ancient.log", 40)
	oldGz := write("kept.log.gz", 40)     // transcripts keep 90 days
	deadGz := write("expired.log.gz", 91) // then go
	art := filepath.Join(dataDir, "artifacts", "a1")
	if err := os.MkdirAll(art, 0o700); err != nil {
		t.Fatal(err)
	}
	mt := now.AddDate(0, 0, -91)
	if err := os.Chtimes(art, mt, mt); err != nil {
		t.Fatal(err)
	}

	rep, err := Prune(context.Background(), PruneInput{DataDir: dataDir, Retention: ret, Now: now, Delete: false})
	if err != nil {
		t.Fatal(err)
	}
	if rep.Outputs != 2 || rep.Compressed != 1 || rep.ArtifactDirs != 1 {
		t.Fatalf("dry run = %+v", rep)
	}
	for _, p := range []string{fresh, toCompress, toDrop, oldGz, deadGz} {
		if _, err := os.Stat(p); err != nil {
			t.Errorf("dry run touched %s: %v", p, err)
		}
	}
	rep, err = Prune(context.Background(), PruneInput{DataDir: dataDir, Retention: ret, Now: now, Delete: true})
	if err != nil {
		t.Fatal(err)
	}
	if rep.Outputs != 2 || rep.Compressed != 1 || rep.ArtifactDirs != 1 {
		t.Fatalf("delete = %+v", rep)
	}
	if _, err := os.Stat(fresh); err != nil {
		t.Error("fresh log removed")
	}
	if _, err := os.Stat(toCompress); !os.IsNotExist(err) {
		t.Error("compressed original still present")
	}
	if info, err := os.Stat(toCompress + ".gz"); err != nil || !info.ModTime().Equal(now.AddDate(0, 0, -10)) {
		t.Errorf("gz mtime not preserved: %v %v", info, err)
	}
	for _, gone := range []string{toDrop, deadGz, art} {
		if _, err := os.Stat(gone); !os.IsNotExist(err) {
			t.Errorf("%s not removed", gone)
		}
	}
	if _, err := os.Stat(oldGz); err != nil {
		t.Error("young transcript gz removed")
	}
	// Empty state: zero deletions (smoke 14's shape).
	rep, err = Prune(context.Background(), PruneInput{DataDir: t.TempDir(), Retention: ret, Now: now, Delete: false})
	if err != nil || rep.Outputs != 0 || rep.ArtifactDirs != 0 || rep.Compressed != 0 {
		t.Errorf("empty prune = %+v, %v", rep, err)
	}
}
