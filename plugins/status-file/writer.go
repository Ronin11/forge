// writer.go: the atomic file write (a reader must never observe a partial
// status.json) and the debounce that folds a burst of journal entries into
// one write.
package main

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"time"
)

// writeAtomic writes data to path via a temp file in the same directory and a
// rename, so every reader sees either the old file or the new one, whole. On
// failure the temp file is removed, its error joined.
func writeAtomic(path string, data []byte) error {
	f, err := os.CreateTemp(filepath.Dir(path), ".status-*.json")
	if err != nil {
		return fmt.Errorf("create temp status file: %w", err)
	}
	tmp := f.Name()
	err = fillTemp(f, data)
	if err == nil {
		err = os.Rename(tmp, path)
		if err == nil {
			return nil
		}
		err = fmt.Errorf("rename status file into place: %w", err)
	}
	if rerr := os.Remove(tmp); rerr != nil && !errors.Is(rerr, fs.ErrNotExist) {
		err = errors.Join(err, rerr)
	}
	return err
}

// fillTemp writes, chmods, and closes the temp file; on a write or chmod
// failure the close error rides along in the join.
func fillTemp(f *os.File, data []byte) error {
	if _, err := f.Write(data); err != nil {
		return errors.Join(fmt.Errorf("write temp status file: %w", err), f.Close())
	}
	// Bars and status lines read this file; CreateTemp's 0600 would hide it
	// from anything not running as the user, so 0644, the mode of a state file.
	if err := f.Chmod(0o644); err != nil {
		return errors.Join(fmt.Errorf("chmod temp status file: %w", err), f.Close())
	}
	if err := f.Close(); err != nil {
		return fmt.Errorf("close temp status file: %w", err)
	}
	return nil
}

// debounceWindow is how long a burst of journal entries coalesces before one
// rewrite; well under the 1 s freshness the smoke expects.
const debounceWindow = 300 * time.Millisecond

// coalesce debounces: the first value on in arms a timer for window; values
// arriving before it fires are folded in (the maximum — cursors only grow);
// when it fires the folded value goes to out. A burst of journal entries
// becomes one file write. Returns when ctx is cancelled or in closes.
func coalesce(ctx context.Context, in <-chan int64, out chan<- int64, window time.Duration) {
	var timer *time.Timer
	var fire <-chan time.Time
	stop := func() {
		if timer != nil {
			timer.Stop()
		}
	}
	var pending int64
	for {
		select {
		case <-ctx.Done():
			stop()
			return
		case v, ok := <-in:
			if !ok {
				stop()
				return
			}
			if v > pending {
				pending = v
			}
			if fire == nil {
				timer = time.NewTimer(window)
				fire = timer.C
			}
		case <-fire:
			fire = nil
			select {
			case out <- pending:
			case <-ctx.Done():
				return
			}
			pending = 0
		}
	}
}
