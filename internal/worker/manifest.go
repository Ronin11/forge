package worker

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"

	"forge/internal/model"
)

// Manifest lifecycles (DESIGN §4.3). The main path is preparing →
// worktree_created → running → exited → cleaned | retained; the side states
// record how an attempt left the path without pretending it finished normally.
const (
	ManifestPreparing       = "preparing"
	ManifestWorktreeCreated = "worktree_created"
	ManifestRunning         = "running"
	ManifestExited          = "exited"
	ManifestCleaned         = "cleaned"
	ManifestRetained        = "retained"
	ManifestNotCreated      = "not_created"
	ManifestInconsistent    = "inconsistent"
	ManifestMissing         = "missing"
	ManifestAwaitingMerge   = "awaiting_merge"
)

// ManifestSchemaVersion is bumped whenever a field changes meaning; a manifest
// from a foreign version is refused rather than reinterpreted.
const ManifestSchemaVersion = 1

// Manifest is the worker's durable record of one attempt, written before any
// mutation it describes (intent first), so a crash between the two leaves
// something reconcile can inspect.
type Manifest struct {
	SchemaVersion   int       `json:"schema_version"`
	WorkerID        string    `json:"worker_id"`
	AttemptID       string    `json:"attempt_id"`
	TargetID        string    `json:"target_id"`
	WorkID          string    `json:"work_id"`
	RoutineName     string    `json:"routine_name"`
	RepositoryName  string    `json:"repository_name"`
	RepositoryPath  string    `json:"repository_path"`
	OriginIdentity  string    `json:"origin_identity"`
	BaseBranch      string    `json:"base_branch,omitempty"`
	BaseCommit      string    `json:"base_commit,omitempty"`
	WorktreePath    string    `json:"worktree_path"`
	Branch          string    `json:"branch"`
	Kind            string    `json:"kind"` // "attempt" | "greenfield" | "merge"
	PID             int       `json:"pid,omitempty"`
	PIDStart        int64     `json:"pid_start,omitempty"` // /proc/<pid>/stat field 22
	ProcessActive   bool      `json:"process_active"`
	Lifecycle       string    `json:"lifecycle"`
	TerminalState   string    `json:"terminal_state,omitempty"`
	RetentionReason string    `json:"retention_reason,omitempty"`
	CleanupIntent   string    `json:"cleanup_intent,omitempty"` // "" | "automatic" | "operator_confirmed"
	CleanupCommand  string    `json:"cleanup_command,omitempty"`
	Resumable       bool      `json:"resumable"`
	SessionID       string    `json:"session_id,omitempty"`
	Launches        int       `json:"launches"`
	ElapsedBeforeUS int64     `json:"elapsed_before_us"`
	NextSeq         int       `json:"next_seq"`
	DeadlineAt      time.Time `json:"deadline_at,omitempty"`
	CreatedAt       time.Time `json:"created_at"`
	UpdatedAt       time.Time `json:"updated_at"`
}

// Final reports whether the manifest may be deleted: only when there is nothing
// left on disk or in Git that the manifest is the sole record of.
func (m *Manifest) Final() bool {
	switch m.Lifecycle {
	case ManifestCleaned, ManifestNotCreated, ManifestMissing:
		return true
	}
	return false
}

// Manifest kinds. An attempt and a merge own a worktree under the data
// directory; a greenfield project lives wherever the operator asked for it.
const (
	manifestKindAttempt    = "attempt"
	manifestKindGreenfield = "greenfield"
	manifestKindMerge      = "merge"
)

// Cleanup intents. Written before a removal so that a crash mid-removal is
// distinguishable from a removal nobody asked for.
const (
	cleanupIntentAutomatic         = "automatic"
	cleanupIntentOperatorConfirmed = "operator_confirmed"
)

var manifestLifecycles = map[string]bool{
	ManifestPreparing:       true,
	ManifestWorktreeCreated: true,
	ManifestRunning:         true,
	ManifestExited:          true,
	ManifestCleaned:         true,
	ManifestRetained:        true,
	ManifestNotCreated:      true,
	ManifestInconsistent:    true,
	ManifestMissing:         true,
	ManifestAwaitingMerge:   true,
}

// CorruptManifest is a manifest file LoadAll could not accept. It is reported,
// never deleted: a manifest that fails validation is exactly the one reconcile
// must not guess about.
type CorruptManifest struct {
	Path string
	Err  error
}

// ManifestStore keeps manifests under <dataDir>/attempts/<attempt-id>.json
// (0600, directory 0700). Writes are a temp file in the same directory + fsync +
// rename + fsync of the directory, so a reader sees either the old manifest or
// the new one, never a torn one.
type ManifestStore struct {
	dataDir  string
	dir      string
	workerID string
	clock    func() time.Time
}

// NewManifestStore creates <dataDir>/attempts with mode 0700 and refuses a
// symlinked directory, because everything the store later trusts (the owned
// worktree path, the manifest files) is derived from this location.
func NewManifestStore(dataDir, workerID string, clock func() time.Time) (*ManifestStore, error) {
	if workerID == "" {
		return nil, errors.New("manifest store: worker id is empty")
	}
	if clock == nil {
		return nil, errors.New("manifest store: clock is nil")
	}
	abs, err := filepath.Abs(dataDir)
	if err != nil {
		return nil, fmt.Errorf("manifest store: resolve data dir %q: %w", dataDir, err)
	}
	dir := filepath.Join(abs, "attempts")
	if err := os.MkdirAll(abs, 0o700); err != nil {
		return nil, fmt.Errorf("manifest store: create data dir: %w", err)
	}
	if err := os.Mkdir(dir, 0o700); err != nil && !errors.Is(err, fs.ErrExist) {
		return nil, fmt.Errorf("manifest store: create attempts dir: %w", err)
	}
	info, err := os.Lstat(dir)
	if err != nil {
		return nil, fmt.Errorf("manifest store: inspect attempts dir: %w", err)
	}
	if info.Mode()&fs.ModeSymlink != 0 {
		return nil, fmt.Errorf("manifest store: attempts dir %s is a symlink", dir)
	}
	if !info.IsDir() {
		return nil, fmt.Errorf("manifest store: attempts path %s is not a directory", dir)
	}
	if err := os.Chmod(dir, 0o700); err != nil {
		return nil, fmt.Errorf("manifest store: restrict attempts dir: %w", err)
	}
	return &ManifestStore{dataDir: abs, dir: dir, workerID: workerID, clock: clock}, nil
}

// Path is where the manifest for attemptID lives; it is only meaningful for an
// ID that passed model.ValidateID, which every caller has done by Write/Load.
func (s *ManifestStore) Path(attemptID string) string {
	return filepath.Join(s.dir, attemptID+".json")
}

// Write stamps the store's identity and clock on m, validates it, and replaces
// the on-disk manifest atomically. m is mutated so the caller's copy matches
// what was persisted.
func (s *ManifestStore) Write(m *Manifest) (err error) {
	now := s.clock().UTC()
	m.SchemaVersion = ManifestSchemaVersion
	m.WorkerID = s.workerID
	if m.CreatedAt.IsZero() {
		m.CreatedAt = now
	}
	m.UpdatedAt = now
	if err := s.Validate(m); err != nil {
		return fmt.Errorf("write manifest: %w", err)
	}
	data, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		return fmt.Errorf("write manifest %s: encode: %w", m.AttemptID, err)
	}
	data = append(data, '\n')

	tmp, err := os.CreateTemp(s.dir, "."+m.AttemptID+"-*.tmp")
	if err != nil {
		return fmt.Errorf("write manifest %s: create temp: %w", m.AttemptID, err)
	}
	tmpPath := tmp.Name()
	committed := false
	defer func() {
		if !committed {
			err = errors.Join(err, removeIfPresent(tmpPath))
		}
	}()
	if err := writeSyncClose(tmp, data); err != nil {
		return fmt.Errorf("write manifest %s: %w", m.AttemptID, err)
	}
	if err := os.Rename(tmpPath, s.Path(m.AttemptID)); err != nil {
		return fmt.Errorf("write manifest %s: rename: %w", m.AttemptID, err)
	}
	committed = true
	if err := syncDir(s.dir); err != nil {
		return fmt.Errorf("write manifest %s: %w", m.AttemptID, err)
	}
	return nil
}

// writeSyncClose is the durable half of an atomic write: the temp file is made
// private, filled, fsynced and closed, and every step's error is reported.
func writeSyncClose(f *os.File, data []byte) (err error) {
	defer func() {
		if cerr := f.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close temp: %w", cerr)
		}
	}()
	if err := f.Chmod(0o600); err != nil {
		return fmt.Errorf("restrict temp: %w", err)
	}
	if _, err := f.Write(data); err != nil {
		return fmt.Errorf("write temp: %w", err)
	}
	if err := f.Sync(); err != nil {
		return fmt.Errorf("sync temp: %w", err)
	}
	return nil
}

// syncDir makes a rename or unlink in dir durable; without it the new name may
// vanish on power loss even though the file data was synced.
func syncDir(dir string) (err error) {
	d, err := os.Open(dir)
	if err != nil {
		return fmt.Errorf("open dir for sync: %w", err)
	}
	defer func() {
		if cerr := d.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close dir after sync: %w", cerr)
		}
	}()
	if err := d.Sync(); err != nil {
		return fmt.Errorf("sync dir: %w", err)
	}
	return nil
}

func removeIfPresent(path string) error {
	if err := os.Remove(path); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("remove %s: %w", path, err)
	}
	return nil
}

// Load reads one manifest with the strictness of DESIGN §7.3: a regular,
// private, non-symlinked file holding exactly one JSON object with no unknown
// fields, whose contents this store would itself have written.
func (s *ManifestStore) Load(attemptID string) (*Manifest, error) {
	if err := model.ValidateID(attemptID); err != nil {
		return nil, fmt.Errorf("load manifest: %w", err)
	}
	m, err := s.loadPath(s.Path(attemptID))
	if err != nil {
		return nil, fmt.Errorf("load manifest %s: %w", attemptID, err)
	}
	if m.AttemptID != attemptID {
		return nil, fmt.Errorf("load manifest %s: attempt id %q does not match file name", attemptID, m.AttemptID)
	}
	return m, nil
}

func (s *ManifestStore) loadPath(path string) (m *Manifest, err error) {
	// O_NOFOLLOW makes the symlink check and the open one step, so a link
	// swapped in between an Lstat and the open cannot be followed.
	f, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NOFOLLOW, 0)
	if err != nil {
		if errors.Is(err, syscall.ELOOP) {
			return nil, fmt.Errorf("%s is a symlink", path)
		}
		return nil, err
	}
	defer func() {
		if cerr := f.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close: %w", cerr)
		}
	}()
	info, err := f.Stat()
	if err != nil {
		return nil, fmt.Errorf("stat: %w", err)
	}
	if !info.Mode().IsRegular() {
		return nil, fmt.Errorf("%s is not a regular file", path)
	}
	if perm := info.Mode().Perm(); perm&0o077 != 0 {
		return nil, fmt.Errorf("%s has mode %04o: group or other access is not allowed", path, perm)
	}
	dec := json.NewDecoder(f)
	dec.DisallowUnknownFields()
	m = &Manifest{}
	if err := dec.Decode(m); err != nil {
		if errors.Is(err, io.EOF) {
			return nil, errors.New("file is empty")
		}
		return nil, fmt.Errorf("decode: %w", err)
	}
	if _, err := dec.Token(); !errors.Is(err, io.EOF) {
		return nil, errors.New("trailing JSON after the manifest object")
	}
	if err := s.Validate(m); err != nil {
		return nil, err
	}
	return m, nil
}

// LoadAll returns every acceptable manifest, sorted by attempt ID, and reports
// the rest as corrupt without touching them. Stale temp files left by a crash
// mid-write are removed: they were never a manifest, so nothing is lost.
func (s *ManifestStore) LoadAll() (manifests []*Manifest, corrupt []CorruptManifest, err error) {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return nil, nil, fmt.Errorf("load manifests: %w", err)
	}
	removed := false
	for _, entry := range entries {
		name := entry.Name()
		path := filepath.Join(s.dir, name)
		switch {
		case strings.HasPrefix(name, ".") && strings.HasSuffix(name, ".tmp"):
			if !entry.Type().IsRegular() {
				return nil, nil, fmt.Errorf("load manifests: stale temp %s is not a regular file", path)
			}
			if err := os.Remove(path); err != nil {
				return nil, nil, fmt.Errorf("load manifests: remove stale temp: %w", err)
			}
			removed = true
		case strings.HasSuffix(name, ".json"):
			attemptID := strings.TrimSuffix(name, ".json")
			m, err := s.Load(attemptID)
			if err != nil {
				corrupt = append(corrupt, CorruptManifest{Path: path, Err: err})
				continue
			}
			manifests = append(manifests, m)
		default:
			corrupt = append(corrupt, CorruptManifest{Path: path, Err: errors.New("unexpected entry in attempts dir")})
		}
	}
	if removed {
		if err := syncDir(s.dir); err != nil {
			return nil, nil, fmt.Errorf("load manifests: %w", err)
		}
	}
	sort.Slice(manifests, func(i, j int) bool { return manifests[i].AttemptID < manifests[j].AttemptID })
	return manifests, corrupt, nil
}

// Remove deletes the manifest for attemptID, but only once it is Final: a
// manifest in any other lifecycle is the only record of something that may
// still exist on disk, and deleting it would turn a retained worktree into an
// orphan nobody can explain.
func (s *ManifestStore) Remove(attemptID string) error {
	m, err := s.Load(attemptID)
	if err != nil {
		return fmt.Errorf("remove manifest: %w", err)
	}
	if !m.Final() {
		return fmt.Errorf("remove manifest %s: refusing in lifecycle %q", attemptID, m.Lifecycle)
	}
	if err := os.Remove(s.Path(attemptID)); err != nil {
		return fmt.Errorf("remove manifest %s: %w", attemptID, err)
	}
	if err := syncDir(s.dir); err != nil {
		return fmt.Errorf("remove manifest %s: %w", attemptID, err)
	}
	return nil
}

// Validate is the one rule for what this store accepts, applied on Write and
// on Load alike, so a manifest can never point cleanup at a path or branch the
// worker does not own.
func (s *ManifestStore) Validate(m *Manifest) error {
	if m.SchemaVersion != ManifestSchemaVersion {
		return fmt.Errorf("schema version %d is not %d", m.SchemaVersion, ManifestSchemaVersion)
	}
	for _, id := range []struct{ field, value string }{
		{"attempt_id", m.AttemptID},
		{"target_id", m.TargetID},
		{"work_id", m.WorkID},
	} {
		if err := model.ValidateID(id.value); err != nil {
			return fmt.Errorf("%s: %w", id.field, err)
		}
	}
	if m.WorkerID != s.workerID {
		return fmt.Errorf("worker_id %q belongs to a different worker (this is %q)", m.WorkerID, s.workerID)
	}
	switch m.Kind {
	case manifestKindAttempt, manifestKindMerge:
		if owned := filepath.Join(s.dataDir, "worktrees", m.AttemptID); m.WorktreePath != owned {
			return fmt.Errorf("worktree_path %q is not the owned path %q", m.WorktreePath, owned)
		}
	case manifestKindGreenfield:
		if !filepath.IsAbs(m.WorktreePath) || filepath.Clean(m.WorktreePath) != m.WorktreePath {
			return fmt.Errorf("worktree_path %q is not a clean absolute path", m.WorktreePath)
		}
	default:
		return fmt.Errorf("kind %q is not attempt, greenfield, or merge", m.Kind)
	}
	if m.Kind == manifestKindAttempt {
		if err := model.ValidateName(m.RoutineName); err != nil {
			return fmt.Errorf("routine_name: %w", err)
		}
		if want := model.BranchName(m.RoutineName, m.AttemptID); m.Branch != want {
			return fmt.Errorf("branch %q is not the derived name %q", m.Branch, want)
		}
	}
	if !manifestLifecycles[m.Lifecycle] {
		return fmt.Errorf("lifecycle %q is not a manifest lifecycle", m.Lifecycle)
	}
	switch m.CleanupIntent {
	case "", cleanupIntentAutomatic, cleanupIntentOperatorConfirmed:
	default:
		return fmt.Errorf("cleanup_intent %q is not \"\", automatic, or operator_confirmed", m.CleanupIntent)
	}
	if m.PID < 0 || m.PIDStart < 0 {
		return fmt.Errorf("process identity pid=%d pid_start=%d is negative", m.PID, m.PIDStart)
	}
	if (m.PID == 0) != (m.PIDStart == 0) {
		return fmt.Errorf("process identity is partial: pid=%d pid_start=%d", m.PID, m.PIDStart)
	}
	if m.ProcessActive && m.PID == 0 {
		return errors.New("process_active without a process identity")
	}
	if m.CreatedAt.IsZero() {
		return errors.New("created_at is zero")
	}
	return nil
}
