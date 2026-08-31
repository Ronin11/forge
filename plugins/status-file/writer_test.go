package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// TestWriteAtomicNeverPartial hammers the file from a writer goroutine while
// the test reads it in a loop: every read must see one of the two payloads
// whole — never a truncated or interleaved file.
func TestWriteAtomicNeverPartial(t *testing.T) {
	path := filepath.Join(t.TempDir(), "status.json")
	pad := bytes.Repeat([]byte("x"), 8192)
	a := []byte(fmt.Sprintf(`{"which":"a","pad":%q}`, pad))
	b := []byte(fmt.Sprintf(`{"which":"b","pad":%q}`, pad))
	if err := writeAtomic(path, a); err != nil {
		t.Fatalf("first write: %v", err)
	}
	done := make(chan error, 1)
	go func() {
		for i := range 400 {
			p := a
			if i%2 == 1 {
				p = b
			}
			if err := writeAtomic(path, p); err != nil {
				done <- err
				return
			}
		}
		done <- nil
	}()
	for {
		select {
		case err := <-done:
			if err != nil {
				t.Fatalf("writer: %v", err)
			}
			return
		default:
		}
		got, err := os.ReadFile(path)
		if err != nil {
			t.Fatalf("read: %v", err)
		}
		if !bytes.Equal(got, a) && !bytes.Equal(got, b) {
			t.Fatalf("observed a partial file (%d bytes)", len(got))
		}
		var v map[string]any
		if err := json.Unmarshal(got, &v); err != nil {
			t.Fatalf("observed invalid JSON: %v", err)
		}
	}
}

func TestWriteAtomicLeavesNoTempFiles(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "status.json")
	if err := writeAtomic(path, []byte(`{}`)); err != nil {
		t.Fatalf("write: %v", err)
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("readdir: %v", err)
	}
	if len(entries) != 1 || entries[0].Name() != "status.json" {
		t.Fatalf("dir holds %v, want only status.json", entries)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if info.Mode().Perm() != 0o644 {
		t.Errorf("mode = %v, want 0644", info.Mode().Perm())
	}
}

// TestCoalesceFoldsBursts sends a burst of cursors and expects exactly one
// output carrying the highest, then a second burst producing a second output.
func TestCoalesceFoldsBursts(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	in := make(chan int64)
	out := make(chan int64, 8)
	go coalesce(ctx, in, out, 50*time.Millisecond)

	deadline := time.After(5 * time.Second)
	for i := int64(1); i <= 10; i++ {
		select {
		case in <- i:
		case <-deadline:
			t.Fatal("send stalled")
		}
	}
	var first int64
	select {
	case first = <-out:
	case <-deadline:
		t.Fatal("no coalesced output")
	}
	if first != 10 {
		t.Fatalf("first output = %d, want the burst's max 10", first)
	}
	// The burst must have been folded into exactly one output.
	select {
	case extra := <-out:
		t.Fatalf("second output %d for one burst", extra)
	case <-time.After(150 * time.Millisecond):
	}
	select {
	case in <- 11:
	case <-deadline:
		t.Fatal("send stalled")
	}
	select {
	case second := <-out:
		if second != 11 {
			t.Fatalf("second output = %d, want 11", second)
		}
	case <-deadline:
		t.Fatal("no output for the second burst")
	}
}
