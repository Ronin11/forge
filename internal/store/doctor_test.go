package store

import (
	"context"
	"testing"
	"time"

	"forge/internal/core/protocol"
	"forge/internal/kb"
)

func TestDoctorReaders(t *testing.T) {
	ctx := context.Background()
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	st, err := Open(ctx, t.TempDir()+"/forge.sqlite3", Options{Clock: func() time.Time { return now }})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	}()

	if n, err := st.RetainedWorktreeCount(ctx); err != nil || n != 0 {
		t.Errorf("fresh retained count = %d, %v", n, err)
	}
	if at, err := st.KbLastIndexedAt(ctx); err != nil || !at.IsZero() {
		t.Errorf("fresh kb indexed at = %v, %v", at, err)
	}

	workerID := "0123456789abcdef0123456789abcdef"
	err = st.Write(ctx, func(tx *Tx) error {
		if err := tx.EnsureProject(ctx, "default"); err != nil {
			return err
		}
		if err := tx.Register(ctx, protocol.RegisterRequest{
			WorkerID: workerID, Name: "local", Version: "test", MaxConcurrent: 1,
			Retained: []protocol.RetainedWorktree{
				{AttemptID: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", Path: "/tmp/wt-a", Reason: "failed"},
				{AttemptID: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", Path: "/tmp/wt-b", Reason: "dirty"},
			},
		}); err != nil {
			return err
		}
		_, err := tx.ReindexKb(ctx, []*kb.Note{{ID: "note-1", Path: "/kb/note-1.md", Title: "n", Type: "note", Created: now, ModTime: now, Size: 3}})
		return err
	})
	if err != nil {
		t.Fatal(err)
	}

	if n, err := st.RetainedWorktreeCount(ctx); err != nil || n != 2 {
		t.Errorf("retained count = %d, %v, want 2", n, err)
	}
	if at, err := st.KbLastIndexedAt(ctx); err != nil || !at.Equal(now) {
		t.Errorf("kb indexed at = %v, %v, want %v", at, err, now)
	}
}
