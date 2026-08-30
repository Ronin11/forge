package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"forge/internal/model"
)

// Routine is a saved procedure. Nil-able numeric fields are pointers so "not set"
// is distinguishable from zero; JSON fields are decoded slices.
type Routine struct {
	ID              string            `json:"id"`
	Name            string            `json:"name"`
	Mode            string            `json:"mode"`
	Prompt          string            `json:"prompt"`
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

// Validate is what the API and CLI reject on before anything is stored.
func (r *Routine) Validate() error {
	if err := model.ValidateName(r.Name); err != nil {
		return err
	}
	if r.Mode == "" {
		return fmt.Errorf("routine %s: mode is required", r.Name)
	}
	if r.Prompt == "" {
		return fmt.Errorf("routine %s: prompt is required", r.Name)
	}
	if r.Model == "" {
		return fmt.Errorf("routine %s: model is required", r.Name)
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
	return nil
}

// applyDefaults fills what Validate requires and the schema defaults.
func (r *Routine) applyDefaults() {
	if r.Executor == "" {
		r.Executor = "claude-code"
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
	_, err = tx.Exec(ctx, `UPDATE routines SET mode=?, prompt=?, repositories=?, executor=?, model=?, effort=?, max_turns=?, timeout_seconds=?, max_budget_usd=?, allowed_tools=?, autonomy=?, verification=?, priority=?, budget_class=?, schedule=?, schedule_enabled=?, concurrency=?, paths=?, deps=?, tier=?, models=?, integrate=?, require_sandbox=?, max_questions=?, generation=?, updated_at=? WHERE name=?`,
		r.Mode, r.Prompt, repos, r.Executor, r.Model, nullString(r.Effort), nullInt(r.MaxTurns), r.TimeoutSeconds, nullFloat(r.MaxBudgetUSD), tools, nullString(string(r.Autonomy)), nullString(r.Verification), r.Priority, string(r.BudgetClass), nullString(r.Schedule), boolInt(r.ScheduleEnabled), r.Concurrency, paths, deps, nullIntPtr(r.Tier), models, boolInt(r.Integrate), boolInt(r.RequireSandbox), r.MaxQuestions, r.Generation, formatTime(r.UpdatedAt), r.Name)
	if err != nil {
		return fmt.Errorf("update routine %s: %w", r.Name, err)
	}
	return tx.recordGeneration(ctx, r, source)
}

func (tx *Tx) insertRoutine(ctx context.Context, r *Routine) error {
	_, err := tx.Exec(ctx, `INSERT INTO routines (id, name, mode, prompt, repositories, executor, model, effort, max_turns, timeout_seconds, max_budget_usd, allowed_tools, autonomy, verification, priority, budget_class, schedule, schedule_enabled, concurrency, paths, deps, tier, models, integrate, require_sandbox, max_questions, generation, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		r.ID, r.Name, r.Mode, r.Prompt, jsonList(r.Repositories), r.Executor, r.Model, nullString(r.Effort), nullInt(r.MaxTurns), r.TimeoutSeconds, nullFloat(r.MaxBudgetUSD), jsonOrNull(r.AllowedTools), nullString(string(r.Autonomy)), nullString(r.Verification), r.Priority, string(r.BudgetClass), nullString(r.Schedule), boolInt(r.ScheduleEnabled), r.Concurrency, jsonOrNull(r.Paths), jsonOrNull(r.Deps), nullIntPtr(r.Tier), jsonOrNull(r.Models), boolInt(r.Integrate), boolInt(r.RequireSandbox), r.MaxQuestions, r.Generation, formatTime(r.CreatedAt), formatTime(r.UpdatedAt))
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

const routineColumns = `id, name, mode, prompt, repositories, executor, model, effort, max_turns, timeout_seconds, max_budget_usd, allowed_tools, autonomy, verification, priority, budget_class, schedule, schedule_enabled, concurrency, paths, deps, tier, models, integrate, require_sandbox, max_questions, generation, next_due_at, archived_at, created_at, updated_at`

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

// DueRoutines returns enabled, unarchived routines whose next_due_at ≤ now.
func (s *Store) DueRoutines(ctx context.Context, now time.Time) ([]Routine, error) {
	return s.routines(each(s.query(ctx, `SELECT `+routineColumns+` FROM routines WHERE schedule_enabled = 1 AND archived_at IS NULL AND next_due_at IS NOT NULL AND next_due_at <= ? ORDER BY next_due_at LIMIT 100`, formatTime(now))))
}

func (s *Store) routines(iter func(func(*sql.Rows) error) error) ([]Routine, error) {
	var out []Routine
	err := iter(func(rows *sql.Rows) error {
		var r Routine
		var effort, tools, autonomy, verification, schedule, paths, deps, models, repos sql.NullString
		var maxTurns, tier sql.NullInt64
		var budget sql.NullFloat64
		var scheduleEnabled, integrate, requireSandbox int
		var nextDue, archived, created, updated sql.NullString
		if err := rows.Scan(&r.ID, &r.Name, &r.Mode, &r.Prompt, &repos, &r.Executor, &r.Model, &effort, &maxTurns, &r.TimeoutSeconds, &budget, &tools, &autonomy, &verification, &r.Priority, &r.BudgetClass, &schedule, &scheduleEnabled, &r.Concurrency, &paths, &deps, &tier, &models, &integrate, &requireSandbox, &r.MaxQuestions, &r.Generation, &nextDue, &archived, &created, &updated); err != nil {
			return fmt.Errorf("scan routine: %w", err)
		}
		r.Effort, r.Verification, r.Schedule = effort.String, verification.String, schedule.String
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
