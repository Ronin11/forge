package worker

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"sync"
	"time"
)

// Manifest lifecycle values. Final values: cleaned, retained, not_created.
const (
	LifecyclePreparing       = "preparing"
	LifecycleWorktreeCreated = "worktree_created"
	LifecycleRunning         = "running"
	LifecycleCompleted       = "completed"
	LifecycleCleaned         = "cleaned"
	LifecycleRetained        = "retained"
	LifecycleNotCreated      = "not_created"
)

var idPattern = regexp.MustCompile(`^[0-9a-f]{32}$`)

// Manifest is the durable record of one attempt, written before launch. It is
// the worker's source of truth for what it started.
type Manifest struct {
	AttemptID   string `json:"attempt_id"`
	TargetID    string `json:"target_id"`
	WorkID      string `json:"work_id"`
	WorkerID    string `json:"worker_id"`
	RoutineName string `json:"routine_name"`

	Repository     string `json:"repository"`
	RepositoryPath string `json:"repository_path"`
	RemoteIdentity string `json:"remote_identity"`
	BaseBranch     string `json:"base_branch"`
	BaseCommit     string `json:"base_commit"`
	WorktreePath   string `json:"worktree_path"`
	Branch         string `json:"branch"`

	PID             int    `json:"pid,omitempty"`
	ProcessIdentity string `json:"process_identity,omitempty"`

	Lifecycle       string    `json:"lifecycle"`
	TerminalState   string    `json:"terminal_state,omitempty"`
	RetentionReason string    `json:"retention_reason,omitempty"`
	CleanupCommand  string    `json:"cleanup_command,omitempty"`
	CreatedAt       time.Time `json:"created_at"`
	UpdatedAt       time.Time `json:"updated_at"`
}

// Final reports whether no further reconciliation applies.
func (m Manifest) Final() bool {
	return m.Lifecycle == LifecycleCleaned || m.Lifecycle == LifecycleRetained || m.Lifecycle == LifecycleNotCreated
}

// ManifestStore reads and writes manifests under dataDir/attempts.
type ManifestStore struct {
	dir string
	mu  sync.Mutex
}

// NewManifestStore creates the attempts directory (0700).
func NewManifestStore(dataDir string) (*ManifestStore, error) {
	dir := filepath.Join(dataDir, "attempts")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, err
	}
	return &ManifestStore{dir: dir}, nil
}

func (s *ManifestStore) path(attemptID string) (string, error) {
	if !idPattern.MatchString(attemptID) {
		return "", fmt.Errorf("invalid attempt id %q", attemptID)
	}
	return filepath.Join(s.dir, attemptID+".json"), nil
}

// Create writes a new manifest; it refuses to overwrite.
func (s *ManifestStore) Create(m Manifest) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	p, err := s.path(m.AttemptID)
	if err != nil {
		return err
	}
	if _, err := os.Lstat(p); err == nil {
		return errors.New("attempt manifest already exists")
	}
	now := time.Now().UTC()
	m.CreatedAt, m.UpdatedAt = now, now
	if err := validateManifest(m); err != nil {
		return err
	}
	return writeAtomic(p, m)
}

// Update applies change to the stored manifest and rewrites it.
func (s *ManifestStore) Update(attemptID string, change func(*Manifest)) (Manifest, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	p, err := s.path(attemptID)
	if err != nil {
		return Manifest{}, err
	}
	m, err := readManifest(p)
	if err != nil {
		return Manifest{}, err
	}
	change(&m)
	m.UpdatedAt = time.Now().UTC()
	if err := validateManifest(m); err != nil {
		return Manifest{}, err
	}
	return m, writeAtomic(p, m)
}

// Load reads one manifest.
func (s *ManifestStore) Load(attemptID string) (Manifest, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	p, err := s.path(attemptID)
	if err != nil {
		return Manifest{}, err
	}
	return readManifest(p)
}

// LoadAll reads every manifest, sorted by creation time. Unreadable files are
// reported in the joined error but do not hide the readable ones.
func (s *ManifestStore) LoadAll() ([]Manifest, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return nil, err
	}
	var out []Manifest
	var errs []error
	for _, e := range entries {
		name := e.Name()
		if e.IsDir() || filepath.Ext(name) != ".json" || name[0] == '.' {
			continue
		}
		m, err := readManifest(filepath.Join(s.dir, name))
		if err != nil {
			errs = append(errs, fmt.Errorf("%s: %w", name, err))
			continue
		}
		out = append(out, m)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].CreatedAt.Before(out[j].CreatedAt) })
	return out, errors.Join(errs...)
}

func readManifest(p string) (Manifest, error) {
	info, err := os.Lstat(p)
	if err != nil {
		return Manifest{}, err
	}
	if !info.Mode().IsRegular() {
		return Manifest{}, errors.New("manifest is not a regular file")
	}
	body, err := os.ReadFile(p)
	if err != nil {
		return Manifest{}, err
	}
	var m Manifest
	if err := json.Unmarshal(body, &m); err != nil {
		return Manifest{}, fmt.Errorf("decode manifest: %w", err)
	}
	return m, validateManifest(m)
}

func validateManifest(m Manifest) error {
	for name, v := range map[string]string{"attempt_id": m.AttemptID, "target_id": m.TargetID, "work_id": m.WorkID} {
		if !idPattern.MatchString(v) {
			return fmt.Errorf("manifest %s %q is not a valid id", name, v)
		}
	}
	if m.Repository == "" || !filepath.IsAbs(m.RepositoryPath) || !filepath.IsAbs(m.WorktreePath) || m.Branch == "" {
		return errors.New("manifest repository, paths, or branch incomplete")
	}
	if !commitPattern.MatchString(m.BaseCommit) {
		return errors.New("manifest base commit is not a full commit")
	}
	switch m.Lifecycle {
	case LifecyclePreparing, LifecycleWorktreeCreated, LifecycleRunning, LifecycleCompleted,
		LifecycleCleaned, LifecycleRetained, LifecycleNotCreated:
	default:
		return fmt.Errorf("manifest lifecycle %q is invalid", m.Lifecycle)
	}
	if (m.PID == 0) != (m.ProcessIdentity == "") {
		return errors.New("manifest process identity is partial")
	}
	return nil
}

// writeAtomic writes JSON to a 0600 temp file, fsyncs, renames, and syncs the
// directory so a crash never leaves a half-written manifest.
func writeAtomic(p string, v any) error {
	body, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return err
	}
	dir := filepath.Dir(p)
	f, err := os.CreateTemp(dir, "."+filepath.Base(p)+".*.tmp")
	if err != nil {
		return err
	}
	tmp := f.Name()
	ok := false
	defer func() {
		if !ok {
			_ = f.Close()
			_ = os.Remove(tmp)
		}
	}()
	if err := f.Chmod(0o600); err != nil {
		return err
	}
	if _, err := f.Write(append(body, '\n')); err != nil {
		return err
	}
	if err := f.Sync(); err != nil {
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	if err := os.Rename(tmp, p); err != nil {
		return err
	}
	ok = true
	d, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer d.Close()
	return d.Sync()
}
