package worker

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"time"

	"forge/internal/core/model"
)

// ResolveAttemptID accepts a full id or a unique prefix among the manifests.
func ResolveAttemptID(dataDir, prefix string) (string, error) {
	entries, err := os.ReadDir(filepath.Join(dataDir, "attempts"))
	if err != nil {
		return "", fmt.Errorf("read manifests: %w", err)
	}
	var matches []string
	for _, e := range entries {
		name := strings.TrimSuffix(e.Name(), ".json")
		if strings.HasSuffix(e.Name(), ".json") && strings.HasPrefix(name, prefix) {
			matches = append(matches, name)
		}
	}
	switch len(matches) {
	case 0:
		return "", fmt.Errorf("no attempt matches %q", prefix)
	case 1:
		return matches[0], nil
	}
	return "", fmt.Errorf("%q matches %d attempts; be more specific", prefix, len(matches))
}

// CleanupPreviewResult is what the operator sees before confirming.
type CleanupPreviewResult struct {
	Path, Branch, Lifecycle, Reason, Status string
	PathExists, Registered, Dirty           bool
}

// CleanupPreview inspects a retained worktree without changing anything.
func CleanupPreview(ctx context.Context, cfg *Config, attemptID string) (*CleanupPreviewResult, error) {
	m, repo, g, err := cleanupLoad(ctx, cfg, attemptID)
	if err != nil {
		return nil, err
	}
	st, err := g.WorktreeState(ctx, repo, m.WorktreePath)
	if err != nil {
		return nil, err
	}
	out := &CleanupPreviewResult{Path: m.WorktreePath, Branch: m.Branch, Lifecycle: m.Lifecycle, Reason: m.RetentionReason, PathExists: st.PathExists, Registered: st.Registered}
	if st.PathExists && st.Registered {
		status, err := g.Run(ctx, m.WorktreePath, "--no-optional-locks", "status", "--porcelain=v1")
		if err != nil {
			return nil, err
		}
		out.Status, out.Dirty = status, strings.TrimSpace(status) != ""
	}
	return out, nil
}

// CleanupConfirm removes the worktree with --force and marks the manifest
// cleaned; it refuses manifests that are not retained or still have a process.
func CleanupConfirm(ctx context.Context, cfg *Config, attemptID string, log *slog.Logger) error {
	m, repo, g, err := cleanupLoad(ctx, cfg, attemptID)
	if err != nil {
		return err
	}
	if m.ProcessActive {
		return fmt.Errorf("attempt %s still has an active process (pid %d); let reconcile resolve it first", model.ShortID(attemptID), m.PID)
	}
	switch m.Lifecycle {
	case ManifestRetained, ManifestInconsistent, ManifestExited, ManifestAwaitingMerge:
	default:
		return fmt.Errorf("attempt %s is %s, not retained", model.ShortID(attemptID), m.Lifecycle)
	}
	store, err := NewManifestStore(cfg.DataDir, m.WorkerID, time.Now)
	if err != nil {
		return err
	}
	m.CleanupIntent = cleanupIntentOperatorConfirmed
	if err := store.Write(m); err != nil {
		return err
	}
	st, err := g.WorktreeState(ctx, repo, m.WorktreePath)
	if err != nil {
		return err
	}
	if st.PathExists || st.Registered {
		if err := g.WorktreeRemove(ctx, repo, m.WorktreePath, true); err != nil {
			return err
		}
	}
	m.Lifecycle = ManifestCleaned
	log.InfoContext(ctx, "worktree removed by operator", "attempt_id", attemptID, "path", m.WorktreePath)
	return store.Write(m)
}

func cleanupLoad(ctx context.Context, cfg *Config, attemptID string) (*Manifest, *Repository, Git, error) {
	id, err := os.ReadFile(filepath.Join(cfg.DataDir, "worker-id"))
	if err != nil {
		return nil, nil, Git{}, fmt.Errorf("read worker id: %w", err)
	}
	store, err := NewManifestStore(cfg.DataDir, string(trimSpace(id)), time.Now)
	if err != nil {
		return nil, nil, Git{}, err
	}
	m, err := store.Load(attemptID)
	if err != nil {
		return nil, nil, Git{}, err
	}
	rc, ok := cfg.Repositories[m.RepositoryName]
	if !ok {
		return nil, nil, Git{}, errors.New("repository " + m.RepositoryName + " is no longer configured")
	}
	g := Git{}
	repo, err := g.ValidateRepository(ctx, m.RepositoryName, rc.Path, rc.BaseBranch)
	if err != nil {
		return nil, nil, Git{}, err
	}
	return m, repo, g, nil
}
