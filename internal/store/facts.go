package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"forge/internal/model"
)

// AttemptFacts is the immutable per-attempt row (DESIGN.md §9.2). Pointer fields
// are NULL when Forge could not compute them; never zero.
type AttemptFacts struct {
	AttemptID         string              `json:"attempt_id"`
	TargetID          string              `json:"target_id"`
	WorkID            string              `json:"work_id"`
	Routine           string              `json:"routine"`
	Generation        int                 `json:"generation"`
	Project           string              `json:"project"`
	Repository        string              `json:"repository"`
	Worker            string              `json:"worker"`
	Executor          string              `json:"executor"`
	Model             string              `json:"model"`
	Effort            string              `json:"effort,omitempty"`
	Mode              string              `json:"mode"`
	Trigger           model.Trigger       `json:"trigger"`
	PromptVersionHash string              `json:"prompt_version_hash,omitempty"`
	Autonomy          model.Autonomy      `json:"autonomy"`
	Phases            map[string]*int64   `json:"phases"` // queue_wait, fetch, … cleanup, total (µs)
	StartedAt         time.Time           `json:"started_at,omitempty"`
	FinishedAt        time.Time           `json:"finished_at"`
	Turns             *int                `json:"turns,omitempty"`
	InputTokens       *int64              `json:"input_tokens,omitempty"`
	OutputTokens      *int64              `json:"output_tokens,omitempty"`
	CacheReadTokens   *int64              `json:"cache_read_tokens,omitempty"`
	CacheCreation     *int64              `json:"cache_creation_tokens,omitempty"`
	CostUSD           *float64            `json:"cost_usd,omitempty"`
	ToolCallsTotal    *int                `json:"tool_calls_total,omitempty"`
	ToolCallsByName   map[string]int      `json:"tool_calls_by_name,omitempty"`
	ToolTimeByName    map[string]int64    `json:"tool_time_us_by_name,omitempty"`
	ToolP50US         *int64              `json:"tool_p50_us,omitempty"`
	ToolMaxUS         *int64              `json:"tool_max_us,omitempty"`
	ToolErrors        *int                `json:"tool_errors,omitempty"`
	QuestionsAsked    *int                `json:"questions_asked,omitempty"`
	WaitHumanUS       *int64              `json:"wait_human_us,omitempty"`
	EventsTotal       *int                `json:"events_total,omitempty"`
	EventsDropped     *int                `json:"events_dropped,omitempty"`
	State             model.State         `json:"state"`
	ExitCode          *int                `json:"exit_code,omitempty"`
	FailureReason     model.FailureReason `json:"failure_reason,omitempty"`
	IsError           *bool               `json:"is_error,omitempty"`
	VerificationLevel *int                `json:"verification_level,omitempty"`
	VerificationPass  *bool               `json:"verification_passed,omitempty"`
	Retained          bool                `json:"retained"`
	RetainedReason    string              `json:"retained_reason,omitempty"`
	Commits           *int                `json:"commits,omitempty"`
	FilesChanged      *int                `json:"files_changed,omitempty"`
	Insertions        *int                `json:"insertions,omitempty"`
	Deletions         *int                `json:"deletions,omitempty"`
	Dirty             *bool               `json:"dirty,omitempty"`
	Pushed            *bool               `json:"pushed,omitempty"`
	Branch            string              `json:"branch,omitempty"`
	Base              string              `json:"base,omitempty"`
	Head              string              `json:"head,omitempty"`
	FiveHourBefore    *float64            `json:"five_hour_before,omitempty"`
	FiveHourAfter     *float64            `json:"five_hour_after,omitempty"`
	SevenDayBefore    *float64            `json:"seven_day_before,omitempty"`
	SevenDayAfter     *float64            `json:"seven_day_after,omitempty"`
	UtilizationDelta  *float64            `json:"utilization_delta_estimate,omitempty"`
}

// PhaseNames are the columns Phases maps to, in order.
var PhaseNames = []string{"queue_wait", "fetch", "resolve_base", "worktree_add", "manifest", "agent", "git_inspect", "verify", "cleanup", "total"}

// InsertFacts writes the row once; a second insert for the same attempt is an
// error, because facts are immutable.
func (tx *Tx) InsertFacts(ctx context.Context, f *AttemptFacts) error {
	byName, err := json.Marshal(f.ToolCallsByName)
	if err != nil {
		return fmt.Errorf("marshal tool calls: %w", err)
	}
	timeByName, err := json.Marshal(f.ToolTimeByName)
	if err != nil {
		return fmt.Errorf("marshal tool time: %w", err)
	}
	phase := func(name string) any {
		if v := f.Phases[name]; v != nil {
			return *v
		}
		return nil
	}
	_, err = tx.Exec(ctx, `INSERT INTO attempt_facts (attempt_id, target_id, work_id, routine, generation, project, repository, worker, executor, model, effort, mode, trigger, prompt_version_hash, autonomy,
		queue_wait_us, fetch_us, resolve_base_us, worktree_add_us, manifest_us, agent_us, git_inspect_us, verify_us, cleanup_us, total_us, started_at, finished_at,
		turns, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd, tool_calls_total, tool_calls_by_name, tool_time_us_by_name, tool_p50_us, tool_max_us, tool_errors, questions_asked, wait_human_us, events_total, events_dropped,
		state, exit_code, failure_reason, is_error, verification_level, verification_passed, retained, retained_reason,
		commits, files_changed, insertions, deletions, dirty, pushed, branch, base, head,
		five_hour_before, five_hour_after, seven_day_before, seven_day_after, utilization_delta_estimate)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		f.AttemptID, f.TargetID, f.WorkID, f.Routine, f.Generation, f.Project, f.Repository, f.Worker, f.Executor, f.Model, nullString(f.Effort), f.Mode, string(f.Trigger), nullString(f.PromptVersionHash), string(f.Autonomy),
		phase("queue_wait"), phase("fetch"), phase("resolve_base"), phase("worktree_add"), phase("manifest"), phase("agent"), phase("git_inspect"), phase("verify"), phase("cleanup"), phase("total"), nullTime(f.StartedAt), formatTime(f.FinishedAt),
		ptrInt(f.Turns), ptrInt64(f.InputTokens), ptrInt64(f.OutputTokens), ptrInt64(f.CacheReadTokens), ptrInt64(f.CacheCreation), nullFloatPtr(f.CostUSD), ptrInt(f.ToolCallsTotal), string(byName), string(timeByName), ptrInt64(f.ToolP50US), ptrInt64(f.ToolMaxUS), ptrInt(f.ToolErrors), ptrInt(f.QuestionsAsked), ptrInt64(f.WaitHumanUS), ptrInt(f.EventsTotal), ptrInt(f.EventsDropped),
		string(f.State), ptrInt(f.ExitCode), nullString(string(f.FailureReason)), ptrBool(f.IsError), ptrInt(f.VerificationLevel), ptrBool(f.VerificationPass), boolInt(f.Retained), nullString(f.RetainedReason),
		ptrInt(f.Commits), ptrInt(f.FilesChanged), ptrInt(f.Insertions), ptrInt(f.Deletions), ptrBool(f.Dirty), ptrBool(f.Pushed), nullString(f.Branch), nullString(f.Base), nullString(f.Head),
		nullFloatPtr(f.FiveHourBefore), nullFloatPtr(f.FiveHourAfter), nullFloatPtr(f.SevenDayBefore), nullFloatPtr(f.SevenDayAfter), nullFloatPtr(f.UtilizationDelta))
	if err != nil {
		if isUniqueViolation(err) {
			return fmt.Errorf("facts for %s already exist: %w", f.AttemptID, ErrConflict)
		}
		return fmt.Errorf("insert facts for %s: %w", f.AttemptID, err)
	}
	return nil
}

func ptrInt(p *int) any {
	if p == nil {
		return nil
	}
	return *p
}

func ptrInt64(p *int64) any {
	if p == nil {
		return nil
	}
	return *p
}

func ptrBool(p *bool) any {
	if p == nil {
		return nil
	}
	return boolInt(*p)
}

// FactsForAttempt reads one facts row, or ErrNotFound.
func (s *Store) FactsForAttempt(ctx context.Context, attemptID string) (*AttemptFacts, error) {
	fs, err := s.scanFacts(each(s.query(ctx, factsSelect+` WHERE attempt_id = ?`, attemptID)))
	if err != nil {
		return nil, err
	}
	if len(fs) == 0 {
		return nil, fmt.Errorf("facts for %s: %w", attemptID, ErrNotFound)
	}
	return &fs[0], nil
}

// FactsSince returns facts finished in [since, until), optionally for one
// routine, oldest first — the stats query's input (indexed on routine, finished).
func (s *Store) FactsSince(ctx context.Context, since, until time.Time, routine string) ([]AttemptFacts, error) {
	q, args := factsSelect+` WHERE finished_at >= ? AND finished_at < ?`, []any{formatTime(since), formatTime(until)}
	if routine != "" {
		q += ` AND routine = ?`
		args = append(args, routine)
	}
	return s.scanFacts(each(s.query(ctx, q+` ORDER BY finished_at`, args...)))
}

const factsSelect = `SELECT attempt_id, target_id, work_id, routine, generation, project, repository, worker, executor, model, effort, mode, trigger, prompt_version_hash, autonomy,
	queue_wait_us, fetch_us, resolve_base_us, worktree_add_us, manifest_us, agent_us, git_inspect_us, verify_us, cleanup_us, total_us, started_at, finished_at,
	turns, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd, tool_calls_total, tool_calls_by_name, tool_time_us_by_name, tool_p50_us, tool_max_us, tool_errors, questions_asked, wait_human_us, events_total, events_dropped,
	state, exit_code, failure_reason, is_error, verification_level, verification_passed, retained, retained_reason,
	commits, files_changed, insertions, deletions, dirty, pushed, branch, base, head,
	five_hour_before, five_hour_after, seven_day_before, seven_day_after, utilization_delta_estimate FROM attempt_facts`

func (s *Store) scanFacts(iter func(func(*sql.Rows) error) error) ([]AttemptFacts, error) {
	var out []AttemptFacts
	err := iter(func(rows *sql.Rows) error {
		var f AttemptFacts
		var effort, promptHash, started, failure, retainedReason, branch, base, head sql.NullString
		phases := make([]sql.NullInt64, len(PhaseNames))
		var finished, byName, timeByName string
		var turns, in, outT, cacheR, cacheC, toolTotal, p50, maxT, toolErr, qAsked, waitH, evTotal, evDropped, exit, isErr, vLevel, vPass, commits, files, ins, del, dirty, pushed sql.NullInt64
		var retained int
		var cost, fhb, fha, sdb, sda, delta sql.NullFloat64
		dest := []any{&f.AttemptID, &f.TargetID, &f.WorkID, &f.Routine, &f.Generation, &f.Project, &f.Repository, &f.Worker, &f.Executor, &f.Model, &effort, &f.Mode, &f.Trigger, &promptHash, &f.Autonomy}
		for i := range phases {
			dest = append(dest, &phases[i])
		}
		dest = append(dest, &started, &finished, &turns, &in, &outT, &cacheR, &cacheC, &cost, &toolTotal, &byName, &timeByName, &p50, &maxT, &toolErr, &qAsked, &waitH, &evTotal, &evDropped,
			&f.State, &exit, &failure, &isErr, &vLevel, &vPass, &retained, &retainedReason, &commits, &files, &ins, &del, &dirty, &pushed, &branch, &base, &head, &fhb, &fha, &sdb, &sda, &delta)
		if err := rows.Scan(dest...); err != nil {
			return fmt.Errorf("scan facts: %w", err)
		}
		f.Effort, f.PromptVersionHash, f.RetainedReason, f.Branch, f.Base, f.Head = effort.String, promptHash.String, retainedReason.String, branch.String, base.String, head.String
		f.FailureReason, f.Retained = model.FailureReason(failure.String), retained == 1
		f.Phases = map[string]*int64{}
		for i, name := range PhaseNames {
			if phases[i].Valid {
				v := phases[i].Int64
				f.Phases[name] = &v
			}
		}
		var err error
		if f.StartedAt, err = parseTime(started); err != nil {
			return err
		}
		if f.FinishedAt, err = parseTime(sql.NullString{String: finished, Valid: true}); err != nil {
			return err
		}
		if err := json.Unmarshal([]byte(byName), &f.ToolCallsByName); err != nil {
			return fmt.Errorf("decode tool calls: %w", err)
		}
		if err := json.Unmarshal([]byte(timeByName), &f.ToolTimeByName); err != nil {
			return fmt.Errorf("decode tool time: %w", err)
		}
		f.Turns, f.ToolCallsTotal, f.ToolErrors, f.QuestionsAsked, f.EventsTotal, f.EventsDropped, f.ExitCode, f.VerificationLevel = intPtr(turns), intPtr(toolTotal), intPtr(toolErr), intPtr(qAsked), intPtr(evTotal), intPtr(evDropped), intPtr(exit), intPtr(vLevel)
		f.Commits, f.FilesChanged, f.Insertions, f.Deletions = intPtr(commits), intPtr(files), intPtr(ins), intPtr(del)
		f.InputTokens, f.OutputTokens, f.CacheReadTokens, f.CacheCreation, f.ToolP50US, f.ToolMaxUS, f.WaitHumanUS = int64Ptr(in), int64Ptr(outT), int64Ptr(cacheR), int64Ptr(cacheC), int64Ptr(p50), int64Ptr(maxT), int64Ptr(waitH)
		f.IsError, f.VerificationPass, f.Dirty, f.Pushed = boolPtr(isErr), boolPtr(vPass), boolPtr(dirty), boolPtr(pushed)
		f.CostUSD, f.FiveHourBefore, f.FiveHourAfter, f.SevenDayBefore, f.SevenDayAfter, f.UtilizationDelta = floatPtr(cost), floatPtr(fhb), floatPtr(fha), floatPtr(sdb), floatPtr(sda), floatPtr(delta)
		out = append(out, f)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read facts: %w", err)
	}
	return out, nil
}

func intPtr(n sql.NullInt64) *int {
	if !n.Valid {
		return nil
	}
	v := int(n.Int64)
	return &v
}

func int64Ptr(n sql.NullInt64) *int64 {
	if !n.Valid {
		return nil
	}
	v := n.Int64
	return &v
}

func boolPtr(n sql.NullInt64) *bool {
	if !n.Valid {
		return nil
	}
	v := n.Int64 == 1
	return &v
}

func floatPtr(f sql.NullFloat64) *float64 {
	if !f.Valid {
		return nil
	}
	v := f.Float64
	return &v
}
