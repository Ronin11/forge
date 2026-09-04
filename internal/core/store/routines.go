package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"forge/internal/core/model"
)

// Routine is a saved trigger: what runs (Target — a directive or a workflow)
// and when (schedule/manual), under which operational envelope. Nil-able
// numeric fields are pointers so "not set" is distinguishable from zero; JSON
// fields are decoded slices.
//
// The content fields (Mode, Prompt, Persona, Model, Effort) exist on the
// struct ONLY for the frozen-snapshot role: materializeRoutine resolves the
// target's content INTO them before the struct marshals into work.snapshot,
// and pre-restructure snapshots and routine_generations decode through them.
// They are never stored on rows (no columns) and Validate rejects them set.
type Routine struct {
	ID   string `json:"id"`
	Name string `json:"name"`
	// Target is what a run invokes: "directive:<name>" | "workflow:<name>".
	Target string `json:"target,omitempty"`
	// Objective is the default {{objective}} for runs this trigger creates;
	// a per-run objective wins.
	Objective string `json:"objective,omitempty"`
	// Snapshot-only content fields (see the type comment): filled by
	// materialization, never stored.
	Mode            string            `json:"mode,omitempty"`
	Prompt          string            `json:"prompt,omitempty"`
	Persona         string            `json:"persona,omitempty"`
	Repositories    []string          `json:"repositories"`
	Executor        string            `json:"executor"`
	Model           string            `json:"model"`
	Effort          string            `json:"effort,omitempty"`
	MaxTurns        int               `json:"max_turns,omitempty"`
	TimeoutSeconds  int               `json:"timeout_seconds"`
	MaxBudgetUSD    float64           `json:"max_budget_usd,omitempty"`
	AllowedTools    []string          `json:"allowed_tools,omitempty"`
	Autonomy        model.Autonomy    `json:"autonomy,omitempty"`
	Verification    string            `json:"verification,omitempty"`
	Priority        int               `json:"priority"`
	BudgetClass     model.BudgetClass `json:"budget_class"`
	Schedule        string            `json:"schedule,omitempty"`
	ScheduleEnabled bool              `json:"schedule_enabled"`
	Concurrency     int               `json:"concurrency"`
	Paths           []string          `json:"paths,omitempty"`
	Deps            []string          `json:"deps,omitempty"`
	Tier            *int              `json:"tier,omitempty"`
	Models          []string          `json:"models,omitempty"`
	Integrate       bool              `json:"integrate"`
	RequireSandbox  bool              `json:"require_sandbox"`
	MaxQuestions    int               `json:"max_questions"`
	Generation      int               `json:"generation"`
	NextDueAt       time.Time         `json:"next_due_at,omitempty"`
	ArchivedAt      time.Time         `json:"archived_at,omitempty"`
	CreatedAt       time.Time         `json:"created_at"`
	UpdatedAt       time.Time         `json:"updated_at"`
}

// Errors callers map to HTTP statuses.
var (
	ErrNotFound        = errors.New("not found")
	ErrConflict        = errors.New("conflict")
	ErrStaleGeneration = errors.New("stale generation")
)

// Validate is what the API and CLI reject on before anything is stored. A
// stored routine is always a trigger: Target names what it invokes; content
// fields must be empty — content lives in the directive (or the workflow).
func (r *Routine) Validate() error {
	if err := model.ValidateName(r.Name); err != nil {
		return err
	}
	if r.Target == "" {
		return fmt.Errorf("routine %s: target is required (directive:<name> or workflow:<name>)", r.Name)
	}
	if _, _, err := ParseTarget(r.Target); err != nil {
		return fmt.Errorf("routine %s: %w", r.Name, err)
	}
	if r.Mode != "" || r.Prompt != "" || r.Persona != "" || r.Model != "" || r.Effort != "" {
		return fmt.Errorf("routine %s: a routine carries no content fields (mode/prompt/persona/model/effort live in the directive)", r.Name)
	}
	if r.TimeoutSeconds <= 0 || r.TimeoutSeconds > 8*3600 {
		return fmt.Errorf("routine %s: timeout_seconds must be in 1..28800", r.Name)
	}
	if r.Concurrency <= 0 || r.Concurrency > 100 {
		return fmt.Errorf("routine %s: concurrency must be in 1..100", r.Name)
	}
	if !r.BudgetClass.Valid() {
		return fmt.Errorf("routine %s: budget_class %q", r.Name, r.BudgetClass)
	}
	if r.Autonomy != "" && !r.Autonomy.Valid() {
		return fmt.Errorf("routine %s: autonomy %q", r.Name, r.Autonomy)
	}
	for _, repo := range r.Repositories {
		if err := model.ValidateName(repo); err != nil {
			return fmt.Errorf("routine %s: repository: %w", r.Name, err)
		}
	}
	if r.MaxQuestions < 0 {
		return fmt.Errorf("routine %s: max_questions must not be negative", r.Name)
	}
	if err := ValidateSchedule(r.Schedule); err != nil {
		return fmt.Errorf("routine %s: %w", r.Name, err)
	}
	return nil
}

// applyDefaults fills what Validate requires and the schema defaults.
func (r *Routine) applyDefaults() {
	if r.Executor == "" {
		r.Executor = "claude-code"
	}
	// The timeout applies to the Works this trigger creates; a sensible
	// default keeps creation from demanding operational trivia.
	if r.TimeoutSeconds == 0 {
		r.TimeoutSeconds = 3600
	}
	if r.Concurrency == 0 {
		r.Concurrency = 1
	}
	if r.BudgetClass == "" {
		r.BudgetClass = model.ClassNormal
	}
	if r.MaxQuestions == 0 {
		r.MaxQuestions = 3
	}
	if r.Priority == 0 {
		r.Priority = 50
	}
}

// CreateRoutine inserts a routine at generation 1 and records the generation.
func (tx *Tx) CreateRoutine(ctx context.Context, r *Routine) error {
	r.applyDefaults()
	if err := r.Validate(); err != nil {
		return err
	}
	if r.ID == "" {
		r.ID = model.NewID()
	}
	r.Generation = 1
	r.CreatedAt, r.UpdatedAt = tx.now, tx.now
	if err := tx.insertRoutine(ctx, r); err != nil {
		return err
	}
	return tx.recordGeneration(ctx, r, "edit")
}

// UpdateRoutine replaces every editable field, requires the caller's expected
// generation (409 otherwise), and bumps the generation.
// UpdateRoutineFrom is UpdateRoutine with an explicit generation source
// (`proposal:<id>` when an approved proposal applies, DESIGN.md §12).
func (tx *Tx) UpdateRoutineFrom(ctx context.Context, r *Routine, expectedGeneration int, source string) error {
	return tx.updateRoutine(ctx, r, expectedGeneration, source)
}

func (tx *Tx) UpdateRoutine(ctx context.Context, r *Routine, expectedGeneration int) error {
	return tx.updateRoutine(ctx, r, expectedGeneration, "edit")
}

func (tx *Tx) updateRoutine(ctx context.Context, r *Routine, expectedGeneration int, source string) error {
	r.applyDefaults()
	if err := r.Validate(); err != nil {
		return err
	}
	var current int
	err := tx.QueryRow(ctx, `SELECT generation FROM routines WHERE name = ?`, r.Name).Scan(&current)
	if errors.Is(err, sql.ErrNoRows) {
		return fmt.Errorf("routine %s: %w", r.Name, ErrNotFound)
	}
	if err != nil {
		return fmt.Errorf("read routine %s: %w", r.Name, err)
	}
	if current != expectedGeneration {
		return fmt.Errorf("routine %s is at generation %d, not %d: %w", r.Name, current, expectedGeneration, ErrStaleGeneration)
	}
	r.Generation = current + 1
	r.UpdatedAt = tx.now
	repos, tools, paths, deps, models := jsonList(r.Repositories), jsonOrNull(r.AllowedTools), jsonOrNull(r.Paths), jsonOrNull(r.Deps), jsonOrNull(r.Models)
	_, err = tx.Exec(ctx, `UPDATE routines SET repositories=?, executor=?, max_turns=?, timeout_seconds=?, max_budget_usd=?, allowed_tools=?, autonomy=?, verification=?, priority=?, budget_class=?, schedule=?, schedule_enabled=?, concurrency=?, paths=?, deps=?, tier=?, models=?, integrate=?, require_sandbox=?, max_questions=?, generation=?, updated_at=?, target=?, objective=? WHERE name=?`,
		repos, r.Executor, nullInt(r.MaxTurns), r.TimeoutSeconds, nullFloat(r.MaxBudgetUSD), tools, nullString(string(r.Autonomy)), nullString(r.Verification), r.Priority, string(r.BudgetClass), nullString(r.Schedule), boolInt(r.ScheduleEnabled), r.Concurrency, paths, deps, nullIntPtr(r.Tier), models, boolInt(r.Integrate), boolInt(r.RequireSandbox), r.MaxQuestions, r.Generation, formatTime(r.UpdatedAt), r.Target, nullString(r.Objective), r.Name)
	if err != nil {
		return fmt.Errorf("update routine %s: %w", r.Name, err)
	}
	return tx.recordGeneration(ctx, r, source)
}

func (tx *Tx) insertRoutine(ctx context.Context, r *Routine) error {
	_, err := tx.Exec(ctx, `INSERT INTO routines (id, name, repositories, executor, max_turns, timeout_seconds, max_budget_usd, allowed_tools, autonomy, verification, priority, budget_class, schedule, schedule_enabled, concurrency, paths, deps, tier, models, integrate, require_sandbox, max_questions, generation, created_at, updated_at, target, objective)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		r.ID, r.Name, jsonList(r.Repositories), r.Executor, nullInt(r.MaxTurns), r.TimeoutSeconds, nullFloat(r.MaxBudgetUSD), jsonOrNull(r.AllowedTools), nullString(string(r.Autonomy)), nullString(r.Verification), r.Priority, string(r.BudgetClass), nullString(r.Schedule), boolInt(r.ScheduleEnabled), r.Concurrency, jsonOrNull(r.Paths), jsonOrNull(r.Deps), nullIntPtr(r.Tier), jsonOrNull(r.Models), boolInt(r.Integrate), boolInt(r.RequireSandbox), r.MaxQuestions, r.Generation, formatTime(r.CreatedAt), formatTime(r.UpdatedAt), r.Target, nullString(r.Objective))
	if err != nil {
		if isUniqueViolation(err) {
			return fmt.Errorf("routine %s already exists: %w", r.Name, ErrConflict)
		}
		return fmt.Errorf("insert routine %s: %w", r.Name, err)
	}
	return nil
}

func (tx *Tx) recordGeneration(ctx context.Context, r *Routine, source string) error {
	snap, err := json.Marshal(r)
	if err != nil {
		return fmt.Errorf("snapshot routine %s: %w", r.Name, err)
	}
	if _, err := tx.Exec(ctx, `INSERT INTO routine_generations (routine_id, generation, snapshot, source, created_at) VALUES (?, ?, ?, ?, ?)`,
		r.ID, r.Generation, string(snap), source, formatTime(tx.now)); err != nil {
		return fmt.Errorf("record generation %d of %s: %w", r.Generation, r.Name, err)
	}
	return nil
}

// ArchiveRoutine disables the schedule and blocks new runs; history stays.
func (tx *Tx) ArchiveRoutine(ctx context.Context, name string) error {
	res, err := tx.Exec(ctx, `UPDATE routines SET archived_at = ?, schedule_enabled = 0, updated_at = ? WHERE name = ? AND archived_at IS NULL`, formatTime(tx.now), formatTime(tx.now), name)
	if err != nil {
		return fmt.Errorf("archive routine %s: %w", name, err)
	}
	return oneRow(res, "routine "+name)
}

// SetScheduleEnabled flips the schedule without a generation bump: it changes
// when the routine runs, not what it does.
func (tx *Tx) SetScheduleEnabled(ctx context.Context, name string, enabled bool) error {
	res, err := tx.Exec(ctx, `UPDATE routines SET schedule_enabled = ?, updated_at = ? WHERE name = ? AND archived_at IS NULL`, boolInt(enabled), formatTime(tx.now), name)
	if err != nil {
		return fmt.Errorf("set schedule for %s: %w", name, err)
	}
	return oneRow(res, "routine "+name)
}

// SetNextDue records the next cron occurrence.
func (tx *Tx) SetNextDue(ctx context.Context, name string, next time.Time) error {
	if _, err := tx.Exec(ctx, `UPDATE routines SET next_due_at = ? WHERE name = ?`, nullTime(next), name); err != nil {
		return fmt.Errorf("set next due for %s: %w", name, err)
	}
	return nil
}

const routineColumns = `id, name, repositories, executor, max_turns, timeout_seconds, max_budget_usd, allowed_tools, autonomy, verification, priority, budget_class, schedule, schedule_enabled, concurrency, paths, deps, tier, models, integrate, require_sandbox, max_questions, generation, next_due_at, archived_at, created_at, updated_at, target, objective`

// GetRoutine reads one routine by name.
func (s *Store) GetRoutine(ctx context.Context, name string) (*Routine, error) {
	rs, err := s.routines(each(s.query(ctx, `SELECT `+routineColumns+` FROM routines WHERE name = ?`, name)))
	if err != nil {
		return nil, err
	}
	if len(rs) == 0 {
		return nil, fmt.Errorf("routine %s: %w", name, ErrNotFound)
	}
	return &rs[0], nil
}

// GetRoutineTx reads inside a write transaction (run-now snapshots).
func (tx *Tx) GetRoutine(ctx context.Context, name string) (*Routine, error) {
	rs, err := (&Store{}).routines(each(tx.Query(ctx, `SELECT `+routineColumns+` FROM routines WHERE name = ?`, name)))
	if err != nil {
		return nil, err
	}
	if len(rs) == 0 {
		return nil, fmt.Errorf("routine %s: %w", name, ErrNotFound)
	}
	return &rs[0], nil
}

// ListRoutines returns routines by name; archived ones only when asked.
func (s *Store) ListRoutines(ctx context.Context, includeArchived bool) ([]Routine, error) {
	q := `SELECT ` + routineColumns + ` FROM routines`
	if !includeArchived {
		q += ` WHERE archived_at IS NULL`
	}
	return s.routines(each(s.query(ctx, q+` ORDER BY name`)))
}

// OpenWorkCountForRoutine counts a routine's unfinished Works — the
// scheduler's skip-if-running check (pending counts too: a queued occurrence
// that has not started yet should not be doubled).
func (s *Store) OpenWorkCountForRoutine(ctx context.Context, routineID string) (int, error) {
	var n int
	if err := s.queryRow(ctx, `SELECT count(*) FROM work WHERE routine_id = ? AND finished_at IS NULL`, routineID).Scan(&n); err != nil {
		return 0, fmt.Errorf("count open work of routine %s: %w", routineID, err)
	}
	return n, nil
}

// DueRoutines returns enabled, unarchived routines whose next_due_at ≤ now.
func (s *Store) DueRoutines(ctx context.Context, now time.Time) ([]Routine, error) {
	return s.routines(each(s.query(ctx, `SELECT `+routineColumns+` FROM routines WHERE schedule_enabled = 1 AND archived_at IS NULL AND next_due_at IS NOT NULL AND next_due_at <= ? ORDER BY next_due_at LIMIT 100`, formatTime(now))))
}

func (s *Store) routines(iter func(func(*sql.Rows) error) error) ([]Routine, error) {
	var out []Routine
	err := iter(func(rows *sql.Rows) error {
		var r Routine
		var tools, autonomy, verification, schedule, paths, deps, models, repos, objective sql.NullString
		var maxTurns, tier sql.NullInt64
		var budget sql.NullFloat64
		var scheduleEnabled, integrate, requireSandbox int
		var nextDue, archived, created, updated sql.NullString
		if err := rows.Scan(&r.ID, &r.Name, &repos, &r.Executor, &maxTurns, &r.TimeoutSeconds, &budget, &tools, &autonomy, &verification, &r.Priority, &r.BudgetClass, &schedule, &scheduleEnabled, &r.Concurrency, &paths, &deps, &tier, &models, &integrate, &requireSandbox, &r.MaxQuestions, &r.Generation, &nextDue, &archived, &created, &updated, &r.Target, &objective); err != nil {
			return fmt.Errorf("scan routine: %w", err)
		}
		r.Verification, r.Schedule, r.Objective = verification.String, schedule.String, objective.String
		r.Autonomy = model.Autonomy(autonomy.String)
		r.MaxTurns, r.MaxBudgetUSD = int(maxTurns.Int64), budget.Float64
		if tier.Valid {
			v := int(tier.Int64)
			r.Tier = &v
		}
		r.ScheduleEnabled, r.Integrate, r.RequireSandbox = scheduleEnabled == 1, integrate == 1, requireSandbox == 1
		var err error
		if r.Repositories, err = jsonStrings(repos); err != nil {
			return err
		}
		if r.AllowedTools, err = jsonStrings(tools); err != nil {
			return err
		}
		if r.Paths, err = jsonStrings(paths); err != nil {
			return err
		}
		if r.Deps, err = jsonStrings(deps); err != nil {
			return err
		}
		if r.Models, err = jsonStrings(models); err != nil {
			return err
		}
		for _, p := range []struct {
			dst *time.Time
			src sql.NullString
		}{{&r.NextDueAt, nextDue}, {&r.ArchivedAt, archived}, {&r.CreatedAt, created}, {&r.UpdatedAt, updated}} {
			if *p.dst, err = parseTime(p.src); err != nil {
				return err
			}
		}
		out = append(out, r)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read routines: %w", err)
	}
	return out, nil
}

// GenerationSnapshot reads the stored JSON snapshot of one routine generation
// (routine_generations, DESIGN.md §12): what the A/B auto-revert restores. The
// snapshot is exactly what recordGeneration wrote, so a revert can only
// reinstate a state that already existed.
func (tx *Tx) GenerationSnapshot(ctx context.Context, routineID string, generation int) (json.RawMessage, error) {
	var snap string
	err := tx.QueryRow(ctx, `SELECT snapshot FROM routine_generations WHERE routine_id = ? AND generation = ?`, routineID, generation).Scan(&snap)
	if isNoRows(err) {
		return nil, fmt.Errorf("generation %d of routine %s: %w", generation, routineID, ErrNotFound)
	}
	if err != nil {
		return nil, fmt.Errorf("read generation %d of routine %s: %w", generation, routineID, err)
	}
	return json.RawMessage(snap), nil
}
