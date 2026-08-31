// state.go persists what the plugin has already ingested and commented on, so
// restarts neither re-create tasks nor re-comment outcomes. The store is a map
// keyed "<github>#<number>", written atomically (temp + rename, 0600) after
// every change. A malformed or absent file starts empty — the plugin logs a
// warning and carries on rather than crashing.
package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
)

// tracked is one issue the plugin is following.
type tracked struct {
	WorkID        string `json:"work_id"`
	Forge         string `json:"forge"`
	Number        int    `json:"number"`
	GitHub        string `json:"github"`
	Status        string `json:"status"` // "ingested" | "done"
	DoneCommented bool   `json:"done_commented"`
}

// Statuses a tracked entry moves through.
const (
	statusIngested = "ingested"
	statusDone     = "done"
)

// state is the in-memory view of state.json plus its path. It is owned by the
// single poller goroutine, so it needs no lock.
type state struct {
	path    string
	entries map[string]tracked
}

// issueKey is the map key for an issue: "<github>#<number>".
func issueKey(github string, number int) string {
	return fmt.Sprintf("%s#%d", github, number)
}

// loadState reads path into memory, tolerating an absent or malformed file (it
// starts empty, logging a warning) so a corrupt state file never wedges the
// plugin.
func loadState(path string, log *slog.Logger) *state {
	s := &state{path: path, entries: map[string]tracked{}}
	if path == "" {
		return s
	}
	b, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return s
	}
	if err != nil {
		log.Warn("read state; starting empty", "path", path, "err", err)
		return s
	}
	var entries map[string]tracked
	if err := json.Unmarshal(b, &entries); err != nil {
		log.Warn("parse state; starting empty", "path", path, "err", err)
		return s
	}
	if entries != nil {
		s.entries = entries
	}
	return s
}

// has reports whether an issue key is already tracked.
func (s *state) has(key string) bool {
	_, ok := s.entries[key]
	return ok
}

// put records or replaces an entry and persists the store.
func (s *state) put(key string, t tracked) error {
	s.entries[key] = t
	return s.save()
}

// save writes the store atomically. A store with no path (never expected in
// production) is a no-op so tests can run without a plugin directory.
func (s *state) save() error {
	if s.path == "" {
		return nil
	}
	data, err := json.MarshalIndent(s.entries, "", "  ")
	if err != nil {
		return fmt.Errorf("encode state: %w", err)
	}
	return writeStateAtomic(s.path, data)
}

// writeStateAtomic writes data to path via a temp file in the same directory
// and a rename, at 0600, so a reader never sees a partial state file and the
// contents (work ids) are not world-readable.
func writeStateAtomic(path string, data []byte) error {
	f, err := os.CreateTemp(filepath.Dir(path), ".state-*.json")
	if err != nil {
		return fmt.Errorf("create temp state file: %w", err)
	}
	tmp := f.Name()
	err = fillStateTemp(f, data)
	if err == nil {
		err = os.Rename(tmp, path)
		if err == nil {
			return nil
		}
		err = fmt.Errorf("rename state file into place: %w", err)
	}
	if rerr := os.Remove(tmp); rerr != nil && !errors.Is(rerr, fs.ErrNotExist) {
		err = errors.Join(err, rerr)
	}
	return err
}

// fillStateTemp writes, chmods 0600, and closes the temp file.
func fillStateTemp(f *os.File, data []byte) error {
	if _, err := f.Write(data); err != nil {
		return errors.Join(fmt.Errorf("write temp state file: %w", err), f.Close())
	}
	if err := f.Chmod(0o600); err != nil {
		return errors.Join(fmt.Errorf("chmod temp state file: %w", err), f.Close())
	}
	if err := f.Close(); err != nil {
		return fmt.Errorf("close temp state file: %w", err)
	}
	return nil
}
