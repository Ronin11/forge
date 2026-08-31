package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"forge/internal/model"
)

// Work is one invocation of a routine (or an ad-hoc task). Everything but
// Priority and FinishedAt is frozen at creation.
type Work struct {
	ID          string            `json:"id"`
	RoutineID   string            `json:"routine_id,omitempty"`
	RoutineName string            `json:"routine_name"`
	Generation  int               `json:"generation"`
	Title       string            `json:"title"`
	Trigger     model.Trigger     `json:"trigger"`
	Snapshot    json.RawMessage   `json:"snapshot"`
	Priority    int               `json:"priority"`
	BudgetClass model.BudgetClass `json:"budget_class"`
	Autonomy    model.Autonomy    `json:"autonomy"`
	Integrate   bool              `json:"integrate"`
	Paths       []string          `json:"paths,omitempty"`
	Deps        []string          `json:"deps,omitempty"`
	Tier        *int              `json:"tier,omitempty"`
	Models      []string          `json:"models,omitempty"`
	PlanBatchID string            `json:"plan_batch_id,omitempty"`
	// WorkflowRunID groups the Works one workflow run instantiated; Name and
	// Step say which workflow and which step this Work is. A run's state is
	// derived from its Works — there is no run row.
	WorkflowRunID string          `json:"workflow_run_id,omitempty"`
	WorkflowName  string          `json:"workflow_name,omitempty"`
	WorkflowStep  string          `json:"workflow_step,omitempty"`
	PromptHash    string          `json:"prompt_hash,omitempty"`
	ScheduledFor  time.Time       `json:"scheduled_for,omitempty"`
	SubmittedBy   string          `json:"submitted_by,omitempty"`
	ExternalRefs  json.RawMessage `json:"external_refs,omitempty"`
	// Provenance (DESIGN.md §3): CausedByWorkID is the Work whose execution
	// created this one (empty for a root); RootWorkID is the root of the tree
	// (own id for a root, else the parent's root), computed by CreateWork, not
	// callers; Cause is the machine label for the reason. plan_batch_id and the
	// snapshot's verify_of are the older, narrower homes of the same links.
	CausedByWorkID string      `json:"caused_by_work_id,omitempty"`
	RootWorkID     string      `json:"root_work_id,omitempty"`
	Cause          model.Cause `json:"cause,omitempty"`
	CreatedAt      time.Time   `json:"created_at"`
	FinishedAt     time.Time   `json:"finished_at,omitempty"`
}

// Target is one repository within one Work.
type Target struct {
	ID               string              `json:"id"`
	WorkID           string              `json:"work_id"`
	Repository       string              `json:"repository"`
	State            model.State         `json:"state"`
	WorkerID         string              `json:"worker_id,omitempty"`
	LeaseExpiresAt   time.Time           `json:"lease_expires_at,omitempty"`
	CancelRequested  bool                `json:"cancel_requested"`
	Retained         bool                `json:"retained"`
	FailureReason    model.FailureReason `json:"failure_reason,omitempty"`
	UnverifiedReason string              `json:"unverified_reason,omitempty"`
	ExternalRefs     json.RawMessage     `json:"external_refs,omitempty"`
	ClaimedAt        time.Time           `json:"claimed_at,omitempty"`
	StartedAt        time.Time           `json:"started_at,omitempty"`
	FinishedAt       time.Time           `json:"finished_at,omitempty"`
	CreatedAt        time.Time           `json:"created_at"`
}

// CreateWork inserts a Work, one pending Target per repository, and its
// dependency edges, refusing cycles, in one transaction, and journals it.
func (tx *Tx) CreateWork(ctx context.Context, w *Work, repositories []string, edges []model.Edge) ([]Target, error) {
	if len(repositories) == 0 {
		return nil, fmt.Errorf("work needs at least one repository")
	}
	if !w.BudgetClass.Valid() || !w.Autonomy.Valid() {
		return nil, fmt.Errorf("work: budget_class %q or autonomy %q invalid", w.BudgetClass, w.Autonomy)
	}
	if !w.Cause.Valid() {
		return nil, fmt.Errorf("work: cause %q invalid", w.Cause)
	}
	if w.ID == "" {
		w.ID = model.NewID()
	}
	w.CreatedAt = tx.now
	// Provenance invariant (DESIGN.md §3): the root is derived here, so callers
	// only set the immediate cause. A caused_by parent must exist; the row's
	// root is the parent's root. A root's root is its own id. A RootWorkID a
	// caller pre-set is honoured only when it agrees with this.
	if w.CausedByWorkID != "" {
		parent, err := tx.GetWork(ctx, w.CausedByWorkID)
		if err != nil {
			return nil, fmt.Errorf("caused_by work %s: %w", w.CausedByWorkID, err)
		}
		root := parent.RootWorkID
		if root == "" {
			root = parent.ID
		}
		if w.RootWorkID != "" && w.RootWorkID != root {
			return nil, fmt.Errorf("work: root_work_id %s contradicts caused_by parent's root %s", w.RootWorkID, root)
		}
		w.RootWorkID = root
	} else {
		if w.RootWorkID != "" && w.RootWorkID != w.ID {
			return nil, fmt.Errorf("work: root_work_id %s set on a root whose id is %s", w.RootWorkID, w.ID)
		}
		w.RootWorkID = w.ID
	}
	open, err := tx.openEdges(ctx)
	if err != nil {
		return nil, err
	}
	for _, e := range edges {
		e.Work = w.ID
		if model.WouldCycle(open, e) {
			return nil, fmt.Errorf("dependency on %s would create a cycle: %w", e.BlockedBy, ErrConflict)
		}
		open = append(open, e)
	}
	_, err = tx.Exec(ctx, `INSERT INTO work (id, routine_id, routine_name, generation, title, trigger, snapshot, priority, budget_class, autonomy, integrate, paths, deps, tier, models, plan_batch_id, workflow_run_id, workflow_name, workflow_step, prompt_hash, scheduled_for, submitted_by, external_refs, caused_by_work_id, root_work_id, cause, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		w.ID, nullString(w.RoutineID), w.RoutineName, w.Generation, w.Title, string(w.Trigger), string(w.Snapshot), w.Priority, string(w.BudgetClass), string(w.Autonomy), boolInt(w.Integrate), jsonOrNull(w.Paths), jsonOrNull(w.Deps), nullIntPtr(w.Tier), jsonOrNull(w.Models), nullString(w.PlanBatchID), nullString(w.WorkflowRunID), nullString(w.WorkflowName), nullString(w.WorkflowStep), nullString(w.PromptHash), nullTime(w.ScheduledFor), nullString(w.SubmittedBy), jsonRaw(w.ExternalRefs), nullString(w.CausedByWorkID), w.RootWorkID, nullString(string(w.Cause)), formatTime(w.CreatedAt))
	if err != nil {
		return nil, fmt.Errorf("insert work: %w", err)
	}
	targets := make([]Target, 0, len(repositories))
	for _, repo := range repositories {
		t := Target{ID: model.NewID(), WorkID: w.ID, Repository: repo, State: model.Pending, CreatedAt: tx.now}
		if _, err := tx.Exec(ctx, `INSERT INTO targets (id, work_id, repository_name, state, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)`,
			t.ID, t.WorkID, t.Repository, string(t.State), formatTime(tx.now), formatTime(tx.now)); err != nil {
			if isUniqueViolation(err) {
				return nil, fmt.Errorf("repository %s listed twice: %w", repo, ErrConflict)
			}
			return nil, fmt.Errorf("insert target for %s: %w", repo, err)
		}
		targets = append(targets, t)
	}
	for _, e := range edges {
		if _, err := tx.Exec(ctx, `INSERT INTO work_dependencies (work_id, blocked_by_work_id, "on", stack_on) VALUES (?, ?, ?, ?)`,
			w.ID, e.BlockedBy, string(e.On), boolInt(e.StackOn)); err != nil {
			return nil, fmt.Errorf("insert dependency on %s: %w", e.BlockedBy, err)
		}
	}
	ids := make([]string, len(targets))
	for i, t := range targets {
		ids[i] = t.ID
	}
	payload := map[string]any{"routine": w.RoutineName, "generation": w.Generation, "trigger": w.Trigger, "targets": ids, "repositories": repositories}
	if w.WorkflowRunID != "" {
		payload["workflow"], payload["workflow_run_id"], payload["workflow_step"] = w.WorkflowName, w.WorkflowRunID, w.WorkflowStep
	}
	if w.CausedByWorkID != "" {
		payload["caused_by"] = w.CausedByWorkID
	}
	if w.Cause != "" {
		payload["cause"] = w.Cause
	}
	payload["root"] = w.RootWorkID
	if err := tx.Journal(ctx, "work.created", EntityWork, w.ID, payload); err != nil {
		return nil, err
	}
	return targets, nil
}

// openEdges returns dependency edges among non-finished Work — the graph that
// matters for cycle checks.
func (tx *Tx) openEdges(ctx context.Context) ([]model.Edge, error) {
	var out []model.Edge
	err := each(tx.Query(ctx, `SELECT d.work_id, d.blocked_by_work_id, d."on", d.stack_on FROM work_dependencies d JOIN work w ON w.id = d.work_id WHERE w.finished_at IS NULL`))(func(rows *sql.Rows) error {
		var e model.Edge
		var on string
		var stack int
		if err := rows.Scan(&e.Work, &e.BlockedBy, &on, &stack); err != nil {
			return err
		}
		e.On, e.StackOn = model.DependencyOn(on), stack == 1
		out = append(out, e)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read dependencies: %w", err)
	}
	return out, nil
}

// SetPriority is the queue's reorder primitive.
func (tx *Tx) SetPriority(ctx context.Context, workID string, priority int) error {
	res, err := tx.Exec(ctx, `UPDATE work SET priority = ? WHERE id = ? AND finished_at IS NULL`, priority, workID)
	if err != nil {
		return fmt.Errorf("set priority of %s: %w", workID, err)
	}
	if err := oneRow(res, "work "+workID); err != nil {
		return err
	}
	return tx.Journal(ctx, "work.priority", EntityWork, workID, map[string]any{"priority": priority})
}

// AddDependency adds a blocked_by edge, refusing cycles.
func (tx *Tx) AddDependency(ctx context.Context, e model.Edge) error {
	open, err := tx.openEdges(ctx)
	if err != nil {
		return err
	}
	if model.WouldCycle(open, e) {
		return fmt.Errorf("dependency %s → %s would create a cycle: %w", e.Work, e.BlockedBy, ErrConflict)
	}
	if _, err := tx.Exec(ctx, `INSERT OR REPLACE INTO work_dependencies (work_id, blocked_by_work_id, "on", stack_on) VALUES (?, ?, ?, ?)`, e.Work, e.BlockedBy, string(e.On), boolInt(e.StackOn)); err != nil {
		return fmt.Errorf("add dependency: %w", err)
	}
	return tx.Journal(ctx, "work.dependency_added", EntityWork, e.Work, e)
}

// RemoveDependency deletes an edge.
func (tx *Tx) RemoveDependency(ctx context.Context, workID, blockedBy string) error {
	res, err := tx.Exec(ctx, `DELETE FROM work_dependencies WHERE work_id = ? AND blocked_by_work_id = ?`, workID, blockedBy)
	if err != nil {
		return fmt.Errorf("remove dependency: %w", err)
	}
	if err := oneRow(res, "dependency of "+workID); err != nil {
		return err
	}
	return tx.Journal(ctx, "work.dependency_removed", EntityWork, workID, map[string]string{"blocked_by": blockedBy})
}

// FinishWork stamps finished_at once, when the last Target went terminal.
func (tx *Tx) FinishWork(ctx context.Context, workID string) error {
	if _, err := tx.Exec(ctx, `UPDATE work SET finished_at = ? WHERE id = ? AND finished_at IS NULL`, formatTime(tx.now), workID); err != nil {
		return fmt.Errorf("finish work %s: %w", workID, err)
	}
	return tx.Journal(ctx, "work.finished", EntityWork, workID, nil)
}

const workColumns = `id, routine_id, routine_name, generation, title, trigger, snapshot, priority, budget_class, autonomy, integrate, paths, deps, tier, models, plan_batch_id, workflow_run_id, workflow_name, workflow_step, prompt_hash, scheduled_for, submitted_by, external_refs, caused_by_work_id, root_work_id, cause, created_at, finished_at`

// GetWork reads one Work.
func (s *Store) GetWork(ctx context.Context, id string) (*Work, error) {
	ws, err := scanWork(each(s.query(ctx, `SELECT `+workColumns+` FROM work WHERE id = ?`, id)))
	if err != nil {
		return nil, err
	}
	if len(ws) == 0 {
		return nil, fmt.Errorf("work %s: %w", id, ErrNotFound)
	}
	return &ws[0], nil
}

// GetWorkTx is GetWork inside a transaction.
func (tx *Tx) GetWork(ctx context.Context, id string) (*Work, error) {
	ws, err := scanWork(each(tx.Query(ctx, `SELECT `+workColumns+` FROM work WHERE id = ?`, id)))
	if err != nil {
		return nil, err
	}
	if len(ws) == 0 {
		return nil, fmt.Errorf("work %s: %w", id, ErrNotFound)
	}
	return &ws[0], nil
}

// OpenWork returns every unfinished Work, oldest first (the queue's input).
func (s *Store) OpenWork(ctx context.Context) ([]Work, error) {
	return scanWork(each(s.query(ctx, `SELECT `+workColumns+` FROM work WHERE finished_at IS NULL ORDER BY created_at`)))
}

// OpenWorkTx is OpenWork inside the claim transaction.
func (tx *Tx) OpenWork(ctx context.Context) ([]Work, error) {
	return scanWork(each(tx.Query(ctx, `SELECT `+workColumns+` FROM work WHERE finished_at IS NULL ORDER BY created_at`)))
}

// ListWork returns the most recent Work, bounded.
func (s *Store) ListWork(ctx context.Context, limit int) ([]Work, error) {
	if limit <= 0 || limit > 500 {
		limit = 50
	}
	return scanWork(each(s.query(ctx, `SELECT `+workColumns+` FROM work ORDER BY created_at DESC LIMIT ?`, limit)))
}

// ListWorkPage returns a page of Work newest-first for the Tasks view: scope
// "open" (finished_at IS NULL — still queued, running, blocked, or waiting),
// "closed" (finished_at set — a terminal state), or "all". limit caps the page
// (default/…max 100/500) and offset walks the pages for infinite scroll.
func (s *Store) ListWorkPage(ctx context.Context, scope string, limit, offset int) ([]Work, error) {
	if limit <= 0 || limit > 500 {
		limit = 100
	}
	if offset < 0 {
		offset = 0
	}
	where := ""
	switch scope {
	case "open":
		where = "WHERE finished_at IS NULL "
	case "closed":
		where = "WHERE finished_at IS NOT NULL "
	}
	return scanWork(each(s.query(ctx, `SELECT `+workColumns+` FROM work `+where+`ORDER BY created_at DESC LIMIT ? OFFSET ?`, limit, offset)))
}

// WorkTree returns every Work in one provenance tree — all rows sharing a
// root_work_id — oldest first. One indexed query is the whole point of the
// denormalized root (DESIGN.md §3 "Provenance").
func (s *Store) WorkTree(ctx context.Context, rootID string) ([]Work, error) {
	return scanWork(each(s.query(ctx, `SELECT `+workColumns+` FROM work WHERE root_work_id = ? ORDER BY created_at`, rootID)))
}

// WorkDependenciesWithin returns every dependency edge whose BOTH ends are in
// the given set of Work ids — the edges to draw inside one lineage tree
// (finished or not, unlike DependencyEdges).
func (s *Store) WorkDependenciesWithin(ctx context.Context, ids []string) ([]model.Edge, error) {
	if len(ids) == 0 {
		return nil, nil
	}
	in := make(map[string]bool, len(ids))
	for _, id := range ids {
		in[id] = true
	}
	q, args := inClause(`SELECT work_id, blocked_by_work_id, "on", stack_on FROM work_dependencies WHERE work_id IN (`, ids, `)`)
	var out []model.Edge
	err := each(s.query(ctx, q, args...))(func(rows *sql.Rows) error {
		var e model.Edge
		var on string
		var stack int
		if err := rows.Scan(&e.Work, &e.BlockedBy, &on, &stack); err != nil {
			return err
		}
		if !in[e.BlockedBy] {
			return nil
		}
		e.On, e.StackOn = model.DependencyOn(on), stack == 1
		out = append(out, e)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read tree dependencies: %w", err)
	}
	return out, nil
}

// ActiveWorkByRoutine counts Work per routine with a Target holding a slot —
// the concurrency rule's input.
func (tx *Tx) ActiveWorkByRoutine(ctx context.Context) (map[string]int, error) {
	out := map[string]int{}
	err := each(tx.Query(ctx, `SELECT w.routine_id, count(DISTINCT w.id) FROM work w JOIN targets t ON t.work_id = w.id WHERE w.routine_id IS NOT NULL AND t.state IN ('claimed','preparing','running') GROUP BY w.routine_id`))(func(rows *sql.Rows) error {
		var id string
		var n int
		if err := rows.Scan(&id, &n); err != nil {
			return err
		}
		out[id] = n
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("count active work: %w", err)
	}
	return out, nil
}

func scanWork(iter func(func(*sql.Rows) error) error) ([]Work, error) {
	var out []Work
	err := iter(func(rows *sql.Rows) error {
		var w Work
		var routineID, paths, deps, models, batch, wfRun, wfName, wfStep, hash, scheduled, submitted, refs, causedBy, root, cause, finished sql.NullString
		var tier sql.NullInt64
		var snapshot, created string
		var integrate int
		if err := rows.Scan(&w.ID, &routineID, &w.RoutineName, &w.Generation, &w.Title, &w.Trigger, &snapshot, &w.Priority, &w.BudgetClass, &w.Autonomy, &integrate, &paths, &deps, &tier, &models, &batch, &wfRun, &wfName, &wfStep, &hash, &scheduled, &submitted, &refs, &causedBy, &root, &cause, &created, &finished); err != nil {
			return fmt.Errorf("scan work: %w", err)
		}
		w.RoutineID, w.PlanBatchID, w.PromptHash, w.SubmittedBy = routineID.String, batch.String, hash.String, submitted.String
		w.WorkflowRunID, w.WorkflowName, w.WorkflowStep = wfRun.String, wfName.String, wfStep.String
		w.CausedByWorkID, w.RootWorkID, w.Cause = causedBy.String, root.String, model.Cause(cause.String)
		w.Snapshot, w.Integrate = json.RawMessage(snapshot), integrate == 1
		if refs.Valid {
			w.ExternalRefs = json.RawMessage(refs.String)
		}
		if tier.Valid {
			v := int(tier.Int64)
			w.Tier = &v
		}
		var err error
		if w.Paths, err = jsonStrings(paths); err != nil {
			return err
		}
		if w.Deps, err = jsonStrings(deps); err != nil {
			return err
		}
		if w.Models, err = jsonStrings(models); err != nil {
			return err
		}
		if w.ScheduledFor, err = parseTime(scheduled); err != nil {
			return err
		}
		if w.CreatedAt, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
			return err
		}
		if w.FinishedAt, err = parseTime(finished); err != nil {
			return err
		}
		out = append(out, w)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read work: %w", err)
	}
	return out, nil
}

const targetColumns = `id, work_id, repository_name, state, worker_id, lease_expires_at, cancel_requested, retained, failure_reason, unverified_reason, external_refs, claimed_at, started_at, finished_at, created_at`

// TargetsForWork returns a Work's Targets.
func (s *Store) TargetsForWork(ctx context.Context, workID string) ([]Target, error) {
	return scanTargets(each(s.query(ctx, `SELECT `+targetColumns+` FROM targets WHERE work_id = ? ORDER BY repository_name`, workID)))
}

// TargetsForWorks fetches Targets for many Works in one query (no N+1 in lists).
func (s *Store) TargetsForWorks(ctx context.Context, workIDs []string) (map[string][]Target, error) {
	out := map[string][]Target{}
	if len(workIDs) == 0 {
		return out, nil
	}
	q, args := inClause(`SELECT `+targetColumns+` FROM targets WHERE work_id IN (`, workIDs, `) ORDER BY work_id, repository_name`)
	ts, err := scanTargets(each(s.query(ctx, q, args...)))
	if err != nil {
		return nil, err
	}
	for _, t := range ts {
		out[t.WorkID] = append(out[t.WorkID], t)
	}
	return out, nil
}

// OpenTargets returns every Target of unfinished Work (the claim transaction's
// view), inside the transaction so the choice and the claim are atomic.
func (tx *Tx) OpenTargets(ctx context.Context) ([]Target, error) {
	return scanTargets(each(tx.Query(ctx, `SELECT `+targetColumns+` FROM targets t WHERE EXISTS (SELECT 1 FROM work w WHERE w.id = t.work_id AND w.finished_at IS NULL) ORDER BY created_at`)))
}

// GetTarget reads one Target.
func (s *Store) GetTarget(ctx context.Context, id string) (*Target, error) {
	ts, err := scanTargets(each(s.query(ctx, `SELECT `+targetColumns+` FROM targets WHERE id = ?`, id)))
	if err != nil {
		return nil, err
	}
	if len(ts) == 0 {
		return nil, fmt.Errorf("target %s: %w", id, ErrNotFound)
	}
	return &ts[0], nil
}

// GetTargetTx is GetTarget inside a transaction.
func (tx *Tx) GetTarget(ctx context.Context, id string) (*Target, error) {
	ts, err := scanTargets(each(tx.Query(ctx, `SELECT `+targetColumns+` FROM targets WHERE id = ?`, id)))
	if err != nil {
		return nil, err
	}
	if len(ts) == 0 {
		return nil, fmt.Errorf("target %s: %w", id, ErrNotFound)
	}
	return &ts[0], nil
}

// DependencyEdges returns every edge of unfinished Work.
func (s *Store) DependencyEdges(ctx context.Context) ([]model.Edge, error) {
	var out []model.Edge
	err := each(s.query(ctx, `SELECT d.work_id, d.blocked_by_work_id, d."on", d.stack_on FROM work_dependencies d JOIN work w ON w.id = d.work_id WHERE w.finished_at IS NULL`))(func(rows *sql.Rows) error {
		var e model.Edge
		var on string
		var stack int
		if err := rows.Scan(&e.Work, &e.BlockedBy, &on, &stack); err != nil {
			return err
		}
		e.On, e.StackOn = model.DependencyOn(on), stack == 1
		out = append(out, e)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read dependencies: %w", err)
	}
	return out, nil
}

func scanTargets(iter func(func(*sql.Rows) error) error) ([]Target, error) {
	var out []Target
	err := iter(func(rows *sql.Rows) error {
		var t Target
		var worker, lease, failure, unverified, refs, claimed, started, finished sql.NullString
		var cancel, retained int
		var created string
		if err := rows.Scan(&t.ID, &t.WorkID, &t.Repository, &t.State, &worker, &lease, &cancel, &retained, &failure, &unverified, &refs, &claimed, &started, &finished, &created); err != nil {
			return fmt.Errorf("scan target: %w", err)
		}
		t.WorkerID, t.UnverifiedReason = worker.String, unverified.String
		t.FailureReason = model.FailureReason(failure.String)
		t.CancelRequested, t.Retained = cancel == 1, retained == 1
		if refs.Valid {
			t.ExternalRefs = json.RawMessage(refs.String)
		}
		var err error
		for _, p := range []struct {
			dst *time.Time
			src sql.NullString
		}{{&t.LeaseExpiresAt, lease}, {&t.ClaimedAt, claimed}, {&t.StartedAt, started}, {&t.FinishedAt, finished}, {&t.CreatedAt, sql.NullString{String: created, Valid: true}}} {
			if *p.dst, err = parseTime(p.src); err != nil {
				return err
			}
		}
		out = append(out, t)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read targets: %w", err)
	}
	return out, nil
}

// inClause builds "prefix ?,?,? suffix" for a list of ids.
func inClause(prefix string, ids []string, suffix string) (string, []any) {
	args := make([]any, len(ids))
	marks := make([]byte, 0, 2*len(ids))
	for i, id := range ids {
		args[i] = id
		if i > 0 {
			marks = append(marks, ',')
		}
		marks = append(marks, '?')
	}
	return prefix + string(marks) + suffix, args
}
