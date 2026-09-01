package store

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	"forge/internal/core/kb"
	"forge/internal/core/model"
)

func TestKbIndexRoundTrip(t *testing.T) {
	ctx := context.Background()
	f := newFixture(t)
	dir := t.TempDir()
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	a := must(kb.WriteNew(dir, kb.New{Title: "Budget policy", Type: "note", Tags: []string{"budget"}, Body: "burn-down window [[reset-notes]]\n", Links: map[string][]string{"about": {"repository:equitizr"}}}, now))
	b := must(kb.WriteNew(dir, kb.New{Title: "Reset notes", Type: "hypothesis", Body: "resets are hourly\n"}, now))
	notes, findings, err := kb.Scan(dir)
	if err != nil || len(findings) != 0 {
		t.Fatalf("scan: %v %v", findings, err)
	}
	var indexed int
	f.write(func(tx *Tx) error {
		indexed, err = tx.ReindexKb(ctx, notes)
		return err
	})
	if indexed != 2 {
		t.Fatalf("indexed %d", indexed)
	}
	// Unchanged files are skipped on the next pass.
	f.write(func(tx *Tx) error {
		indexed, err = tx.ReindexKb(ctx, notes)
		return err
	})
	if indexed != 0 {
		t.Errorf("reindexed unchanged notes: %d", indexed)
	}
	hits := must(f.s.SearchKb(ctx, "burn-down", 10))
	if len(hits) != 1 || hits[0].ID != a.ID || hits[0].Tags[0] != "budget" {
		t.Errorf("search = %+v", hits)
	}
	if hits := must(f.s.SearchKb(ctx, `"burn OR"`, 10)); len(hits) != 0 {
		t.Errorf("fts syntax must be inert: %+v", hits)
	}
	back := must(f.s.KbBacklinks(ctx, kb.Ref{Kind: "note", Val: b.ID}))
	if len(back) != 1 || back[0].FromID != a.ID || back[0].LinkType != "inline" {
		t.Errorf("backlinks = %+v", back)
	}
	back = must(f.s.KbBacklinks(ctx, kb.Ref{Kind: "repository", Val: "equitizr"}))
	if len(back) != 1 || back[0].LinkType != "about" {
		t.Errorf("fact backlinks = %+v", back)
	}
	links := must(f.s.KbLinks(ctx, a.ID))
	if len(links) != 2 {
		t.Errorf("links = %+v", links)
	}
	// A changed file reindexes; a removed file drops out.
	time.Sleep(10 * time.Millisecond) // ensure a new mtime
	if err := os.WriteFile(a.Path, []byte("---\nid: "+a.ID+"\ntitle: \"Budget policy\"\ntype: note\ncreated: 2026-08-30T12:00:00Z\n---\nrewritten body\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(filepath.Join(dir, b.ID+".md")); err != nil {
		t.Fatal(err)
	}
	notes, _, err = kb.Scan(dir)
	if err != nil {
		t.Fatal(err)
	}
	f.write(func(tx *Tx) error {
		indexed, err = tx.ReindexKb(ctx, notes)
		return err
	})
	if indexed != 1 {
		t.Errorf("changed note not reindexed: %d", indexed)
	}
	if _, err := f.s.KbNoteByID(ctx, b.ID); err == nil {
		t.Error("removed note still indexed")
	}
	if hits := must(f.s.SearchKb(ctx, "rewritten", 10)); len(hits) != 1 {
		t.Errorf("new body not searchable: %+v", hits)
	}
	if hits := must(f.s.SearchKb(ctx, "burn-down", 10)); len(hits) != 0 {
		t.Errorf("old body still searchable: %+v", hits)
	}
}

func TestKbFactExists(t *testing.T) {
	ctx := context.Background()
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	a := f.claim(target, "req-kb")
	cases := []struct {
		ref  kb.Ref
		want bool
	}{
		{kb.Ref{Kind: "attempt", Val: a.ID}, true},
		{kb.Ref{Kind: "attempt", Val: "00000000000000000000000000000000"}, false},
		{kb.Ref{Kind: "target", Val: target.ID}, true},
		{kb.Ref{Kind: "work", Val: target.WorkID}, true},
		{kb.Ref{Kind: "routine", Val: "inventory"}, true},
		{kb.Ref{Kind: "routine", Val: "inventory@1"}, true},
		{kb.Ref{Kind: "routine", Val: "inventory@9"}, false},
		{kb.Ref{Kind: "repository", Val: "equitizr"}, true},
		{kb.Ref{Kind: "project", Val: "default"}, true},
		{kb.Ref{Kind: "prompt", Val: "no-such"}, false},
	}
	for _, c := range cases {
		got := must(f.s.KbFactExists(ctx, c.ref))
		if got != c.want {
			t.Errorf("KbFactExists(%+v) = %v", c.ref, got)
		}
	}
	if _, err := f.s.KbFactExists(ctx, kb.Ref{Kind: "bogus"}); err == nil {
		t.Error("unknown kind accepted")
	}
}
