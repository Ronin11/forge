package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
)

// Worker is a registered worker.
type Worker struct {
	ID            string            `json:"id"`
	Name          string            `json:"name"`
	Version       string            `json:"version"`
	MaxConcurrent int               `json:"max_concurrent"`
	Active        int               `json:"active"`
	Executors     []string          `json:"executors"`
	Capabilities  map[string]string `json:"capabilities"`
	RegisteredAt  time.Time         `json:"registered_at"`
	LastSeenAt    time.Time         `json:"last_seen_at"`
	Connected     bool              `json:"connected"`
}

// ConnectedWindow is how long after its last registration or heartbeat a worker
// counts as connected.
const ConnectedWindow = 90 * time.Second

// Register upserts the worker, its repositories, and its retained worktrees.
func (tx *Tx) Register(ctx context.Context, req protocol.RegisterRequest) error {
	if err := model.ValidateID(req.WorkerID); err != nil {
		return fmt.Errorf("worker id: %w", err)
	}
	if err := model.ValidateName(req.Name); err != nil {
		return fmt.Errorf("worker name: %w", err)
	}
	execs, err := json.Marshal(req.Executors)
	if err != nil {
		return fmt.Errorf("marshal executors: %w", err)
	}
	caps, err := json.Marshal(req.Capabilities)
	if err != nil {
		return fmt.Errorf("marshal capabilities: %w", err)
	}
	now := formatTime(tx.now)
	if _, err := tx.Exec(ctx, `INSERT INTO workers (id, name, version, max_concurrent, active, executors, capabilities, registered_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(id) DO UPDATE SET name = excluded.name, version = excluded.version, max_concurrent = excluded.max_concurrent, active = excluded.active, executors = excluded.executors, capabilities = excluded.capabilities, last_seen_at = excluded.last_seen_at`,
		req.WorkerID, req.Name, req.Version, req.MaxConcurrent, req.Active, string(execs), string(caps), now, now); err != nil {
		return fmt.Errorf("register worker %s: %w", req.Name, err)
	}
	for _, r := range req.Repositories {
		if err := model.ValidateName(r.Name); err != nil {
			return fmt.Errorf("repository: %w", err)
		}
		project := r.Project
		if project == "" {
			project = "default"
		}
		var projectID string
		if err := tx.QueryRow(ctx, `SELECT id FROM projects WHERE name = ?`, project).Scan(&projectID); err != nil {
			if isNoRows(err) {
				return fmt.Errorf("repository %s: project %s: %w", r.Name, project, ErrNotFound)
			}
			return fmt.Errorf("read project %s: %w", project, err)
		}
		if _, err := tx.Exec(ctx, `INSERT INTO repositories (name, project_id, path, origin_identity, base_branch, worker_id, last_seen_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
			ON CONFLICT(name) DO UPDATE SET project_id = excluded.project_id, path = excluded.path, origin_identity = excluded.origin_identity, base_branch = excluded.base_branch, worker_id = excluded.worker_id, last_seen_at = excluded.last_seen_at, updated_at = excluded.updated_at`,
			r.Name, projectID, r.Path, r.OriginIdentity, nullString(r.BaseBranch), req.WorkerID, now, now, now); err != nil {
			return fmt.Errorf("register repository %s: %w", r.Name, err)
		}
	}
	if _, err := tx.Exec(ctx, `DELETE FROM retained_worktrees WHERE worker_id = ?`, req.WorkerID); err != nil {
		return fmt.Errorf("clear retained worktrees: %w", err)
	}
	for _, rw := range req.Retained {
		if _, err := tx.Exec(ctx, `INSERT OR REPLACE INTO retained_worktrees (attempt_id, worker_id, path, reason, cleanup_command, reported_at) VALUES (?, ?, ?, ?, ?, ?)`,
			rw.AttemptID, req.WorkerID, rw.Path, rw.Reason, rw.CleanupCommand, now); err != nil {
			return fmt.Errorf("record retained worktree %s: %w", rw.AttemptID, err)
		}
	}
	return nil
}

// TouchWorker marks a worker seen (a heartbeat counts as liveness).
func (tx *Tx) TouchWorker(ctx context.Context, workerID string) error {
	if _, err := tx.Exec(ctx, `UPDATE workers SET last_seen_at = ? WHERE id = ?`, formatTime(tx.now), workerID); err != nil {
		return fmt.Errorf("touch worker %s: %w", workerID, err)
	}
	return nil
}

// Workers lists every worker with its connected flag as of now.
func (s *Store) Workers(ctx context.Context, now time.Time) ([]Worker, error) {
	return scanWorkers(each(s.query(ctx, `SELECT id, name, version, max_concurrent, active, executors, capabilities, registered_at, last_seen_at FROM workers ORDER BY name`)), now)
}

// WorkersTx is Workers inside the claim transaction.
func (tx *Tx) Workers(ctx context.Context) ([]Worker, error) {
	return scanWorkers(each(tx.Query(ctx, `SELECT id, name, version, max_concurrent, active, executors, capabilities, registered_at, last_seen_at FROM workers ORDER BY name`)), tx.now)
}

func scanWorkers(iter func(func(*sql.Rows) error) error, now time.Time) ([]Worker, error) {
	var out []Worker
	err := iter(func(rows *sql.Rows) error {
		var w Worker
		var execs, caps, registered, seen string
		if err := rows.Scan(&w.ID, &w.Name, &w.Version, &w.MaxConcurrent, &w.Active, &execs, &caps, &registered, &seen); err != nil {
			return fmt.Errorf("scan worker: %w", err)
		}
		if err := json.Unmarshal([]byte(execs), &w.Executors); err != nil {
			return fmt.Errorf("decode executors of %s: %w", w.Name, err)
		}
		if err := json.Unmarshal([]byte(caps), &w.Capabilities); err != nil {
			return fmt.Errorf("decode capabilities of %s: %w", w.Name, err)
		}
		var err error
		if w.RegisteredAt, err = parseTime(sql.NullString{String: registered, Valid: true}); err != nil {
			return err
		}
		if w.LastSeenAt, err = parseTime(sql.NullString{String: seen, Valid: true}); err != nil {
			return err
		}
		w.Connected = now.Sub(w.LastSeenAt) <= ConnectedWindow
		out = append(out, w)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read workers: %w", err)
	}
	return out, nil
}

// Repository is a registered checkout as the daemon knows it.
type Repository struct {
	Name           string    `json:"name"`
	Project        string    `json:"project"`
	Path           string    `json:"path"`
	OriginIdentity string    `json:"origin_identity"`
	BaseBranch     string    `json:"base_branch,omitempty"`
	WorkerID       string    `json:"worker_id,omitempty"`
	ForgeToml      string    `json:"forge_toml,omitempty"`
	LastSeenAt     time.Time `json:"last_seen_at,omitempty"`
}

const repositoryColumns = `r.name, p.name, r.path, r.origin_identity, r.base_branch, r.worker_id, r.forge_toml, r.last_seen_at`

// Repositories lists registered repositories by name.
func (s *Store) Repositories(ctx context.Context) ([]Repository, error) {
	return scanRepositories(each(s.query(ctx, `SELECT `+repositoryColumns+` FROM repositories r JOIN projects p ON p.id = r.project_id ORDER BY r.name`)))
}

// RepositoriesTx is Repositories inside a transaction.
func (tx *Tx) Repositories(ctx context.Context) ([]Repository, error) {
	return scanRepositories(each(tx.Query(ctx, `SELECT `+repositoryColumns+` FROM repositories r JOIN projects p ON p.id = r.project_id ORDER BY r.name`)))
}

func scanRepositories(iter func(func(*sql.Rows) error) error) ([]Repository, error) {
	var out []Repository
	err := iter(func(rows *sql.Rows) error {
		var r Repository
		var base, worker, toml, seen sql.NullString
		if err := rows.Scan(&r.Name, &r.Project, &r.Path, &r.OriginIdentity, &base, &worker, &toml, &seen); err != nil {
			return fmt.Errorf("scan repository: %w", err)
		}
		r.BaseBranch, r.WorkerID, r.ForgeToml = base.String, worker.String, toml.String
		var err error
		if r.LastSeenAt, err = parseTime(seen); err != nil {
			return err
		}
		out = append(out, r)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read repositories: %w", err)
	}
	return out, nil
}

// Project is a grouping of repositories with defaults.
type Project struct {
	ID               string            `json:"id"`
	Name             string            `json:"name"`
	Autonomy         model.Autonomy    `json:"autonomy,omitempty"`
	BudgetClass      model.BudgetClass `json:"budget_class"`
	PriorityBaseline int               `json:"priority_baseline"`
}

// EnsureProject creates a project if it does not exist (bootstrap's default).
func (tx *Tx) EnsureProject(ctx context.Context, name string) error {
	if err := model.ValidateName(name); err != nil {
		return err
	}
	if _, err := tx.Exec(ctx, `INSERT OR IGNORE INTO projects (id, name, budget_class, priority_baseline, created_at, updated_at) VALUES (?, ?, 'normal', 50, ?, ?)`,
		model.NewID(), name, formatTime(tx.now), formatTime(tx.now)); err != nil {
		return fmt.Errorf("ensure project %s: %w", name, err)
	}
	return nil
}

// Projects lists projects by name.
func (s *Store) Projects(ctx context.Context) ([]Project, error) {
	var out []Project
	err := each(s.query(ctx, `SELECT id, name, autonomy, budget_class, priority_baseline FROM projects ORDER BY name`))(func(rows *sql.Rows) error {
		var p Project
		var autonomy sql.NullString
		if err := rows.Scan(&p.ID, &p.Name, &autonomy, &p.BudgetClass, &p.PriorityBaseline); err != nil {
			return fmt.Errorf("scan project: %w", err)
		}
		p.Autonomy = model.Autonomy(autonomy.String)
		out = append(out, p)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read projects: %w", err)
	}
	return out, nil
}

// ProjectForRepository returns the project a repository belongs to.
func (tx *Tx) ProjectForRepository(ctx context.Context, repo string) (*Project, error) {
	var p Project
	var autonomy sql.NullString
	err := tx.QueryRow(ctx, `SELECT p.id, p.name, p.autonomy, p.budget_class, p.priority_baseline FROM projects p JOIN repositories r ON r.project_id = p.id WHERE r.name = ?`, repo).Scan(&p.ID, &p.Name, &autonomy, &p.BudgetClass, &p.PriorityBaseline)
	if isNoRows(err) {
		return nil, fmt.Errorf("repository %s: %w", repo, ErrNotFound)
	}
	if err != nil {
		return nil, fmt.Errorf("read project of %s: %w", repo, err)
	}
	p.Autonomy = model.Autonomy(autonomy.String)
	return &p, nil
}
