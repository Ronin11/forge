package controlplane

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"

	"forge/internal/model"
	"forge/internal/protocol"
	"forge/internal/store"
)

// Ad-hoc Work defaults (POST /api/v1/tasks without a routine).
const (
	adHocRoutineName  = "ad-hoc"
	adHocMode         = "run"
	adHocModel        = "haiku"
	adHocExecutor     = "claude-code"
	adHocTimeout      = 1800
	adHocMaxTurns     = 30
	adHocPriority     = 100
	adHocMaxQuestions = 3
	titleMaxRunes     = 80
)

// routineWriteError maps what CreateRoutine and UpdateRoutine return: the
// sentinels keep their status; anything else came from Validate, which runs
// before the row is touched, so it is the client's (a driver failure on the
// insert itself would be misreported as 400; its message still names it).
func routineWriteError(err error) error {
	if err == nil || errors.Is(err, store.ErrNotFound) || errors.Is(err, store.ErrConflict) || errors.Is(err, store.ErrStaleGeneration) {
		return err
	}
	return badRequest("%v", err)
}

func (s *Server) listRoutines(r *http.Request) (int, any, error) {
	routines, err := s.store.ListRoutines(r.Context(), r.URL.Query().Get("archived") == "true")
	if err != nil {
		return 0, nil, err
	}
	if routines == nil {
		routines = []store.Routine{}
	}
	return http.StatusOK, routines, nil
}

// decodeRoutine reads a routine body and rejects what the API decides before
// the store does: the name, and a model alias the daemon cannot resolve.
func (s *Server) decodeRoutine(r *http.Request) (*store.Routine, error) {
	var rt store.Routine
	if err := decodeJSON(r, &rt); err != nil {
		return nil, err
	}
	rt.ID, rt.Generation = "", 0
	if err := model.ValidateName(rt.Name); err != nil {
		return nil, badRequest("%v", err)
	}
	if rt.Model == "" {
		return nil, badRequest("routine %s: model is required", rt.Name)
	}
	if _, ok := s.resolveModel(rt.Model); !ok {
		return nil, badRequest("unknown model alias %q", rt.Model)
	}
	return &rt, nil
}

func (s *Server) createRoutine(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	rt, err := s.decodeRoutine(r)
	if err != nil {
		return 0, nil, err
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return routineWriteError(tx.CreateRoutine(ctx, rt)) }); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "routine created", "routine", rt.Name, "routine_id", rt.ID)
	return http.StatusCreated, rt, nil
}

func (s *Server) getRoutine(r *http.Request) (int, any, error) {
	rt, err := s.store.GetRoutine(r.Context(), r.PathValue("name"))
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, rt, nil
}

func (s *Server) updateRoutine(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	generation, err := strconv.Atoi(r.URL.Query().Get("generation"))
	if err != nil || generation <= 0 {
		return 0, nil, badRequest("generation query parameter is required: the generation you edited")
	}
	rt, err := s.decodeRoutine(r)
	if err != nil {
		return 0, nil, err
	}
	if rt.Name != r.PathValue("name") {
		return 0, nil, badRequest("routine name %q does not match the path", rt.Name)
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		saved, err := tx.GetRoutine(ctx, rt.Name)
		if err != nil {
			return err
		}
		rt.ID = saved.ID // the generation record is keyed by it
		return routineWriteError(tx.UpdateRoutine(ctx, rt, generation))
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "routine updated", "routine", rt.Name, "generation", rt.Generation)
	return http.StatusOK, rt, nil
}

func (s *Server) archiveRoutine(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	name := r.PathValue("name")
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.ArchiveRoutine(ctx, name) }); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "routine archived", "routine", name)
	return http.StatusNoContent, nil, nil
}

// runRequest is POST /api/v1/routines/{name}/run's optional body.
type runRequest struct {
	Repositories []string `json:"repositories"`
}

func (s *Server) runRoutine(r *http.Request) (int, any, error) {
	var body runRequest
	if r.ContentLength != 0 {
		if err := decodeJSON(r, &body); err != nil {
			return 0, nil, err
		}
	}
	return s.submitWork(r.Context(), workRequest{Routine: r.PathValue("name"), Repositories: body.Repositories})
}

// workRequest is POST /api/v1/work|tasks: a routine run with overrides, or an
// ad-hoc task built from the fields.
type workRequest struct {
	Prompt       string            `json:"prompt"`
	Repositories []string          `json:"repositories"`
	Mode         string            `json:"mode"`
	Routine      string            `json:"routine"`
	Priority     *int              `json:"priority"`
	Class        model.BudgetClass `json:"class"`
	Autonomy     model.Autonomy    `json:"autonomy"`
	Model        string            `json:"model"`
	After        []string          `json:"after"`
	Paths        []string          `json:"paths"`
	Integrate    bool              `json:"integrate"`
	Title        string            `json:"title"`
}

// workCreated is the 201 body.
type workCreated struct {
	Work    store.Work     `json:"work"`
	Targets []store.Target `json:"targets"`
}

func (s *Server) createWork(r *http.Request) (int, any, error) {
	var req workRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	return s.submitWork(r.Context(), req)
}

func (s *Server) submitWork(ctx context.Context, req workRequest) (int, any, error) {
	if s.Draining() {
		return 0, nil, errDraining
	}
	var out workCreated
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		var err error
		out, err = s.createWorkTx(ctx, tx, req)
		return err
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "work created", "work_id", out.Work.ID, "routine", out.Work.RoutineName, "generation", out.Work.Generation, "targets", len(out.Targets), "priority", out.Work.Priority, "class", out.Work.BudgetClass)
	return http.StatusCreated, out, nil
}

// createWorkTx freezes the routine (or the ad-hoc fields as a routine) into a
// Work snapshot and creates its Targets. Repositories must be registered;
// `after` ids must exist; the model alias must resolve.
func (s *Server) createWorkTx(ctx context.Context, tx *store.Tx, req workRequest) (workCreated, error) {
	var rt store.Routine
	var w store.Work
	if req.Routine != "" {
		saved, err := tx.GetRoutine(ctx, req.Routine)
		if err != nil {
			return workCreated{}, err
		}
		if !saved.ArchivedAt.IsZero() {
			return workCreated{}, fmt.Errorf("routine %s is archived: %w", saved.Name, store.ErrConflict)
		}
		rt = *saved
		if req.Prompt != "" {
			rt.Prompt = req.Prompt
		}
		if req.Mode != "" {
			rt.Mode = req.Mode
		}
		if req.Model != "" {
			rt.Model = req.Model
		}
		if len(req.Paths) > 0 {
			rt.Paths = req.Paths
		}
		rt.Integrate = rt.Integrate || req.Integrate
		w = store.Work{RoutineID: saved.ID, RoutineName: saved.Name, Generation: saved.Generation, Tier: saved.Tier, Models: saved.Models, Deps: saved.Deps}
	} else {
		if strings.TrimSpace(req.Prompt) == "" {
			return workCreated{}, badRequest("prompt is required")
		}
		rt = store.Routine{
			Name: adHocRoutineName, Mode: adHocMode, Prompt: req.Prompt, Repositories: req.Repositories, Executor: adHocExecutor, Model: adHocModel,
			TimeoutSeconds: adHocTimeout, MaxTurns: adHocMaxTurns, BudgetClass: model.ClassInteractive, Priority: adHocPriority, Concurrency: 1,
			Paths: req.Paths, Integrate: req.Integrate, MaxQuestions: adHocMaxQuestions,
		}
		if req.Mode != "" {
			rt.Mode = req.Mode
		}
		if req.Model != "" {
			rt.Model = req.Model
		}
		w = store.Work{RoutineName: adHocRoutineName}
	}
	if req.Class != "" {
		if !req.Class.Valid() {
			return workCreated{}, badRequest("class %q: want interactive, normal, or backlog", req.Class)
		}
		rt.BudgetClass = req.Class
	}
	if req.Priority != nil {
		rt.Priority = *req.Priority
	}
	if req.Autonomy != "" && !req.Autonomy.Valid() {
		return workCreated{}, badRequest("autonomy %q: want ask, checkpoint, notify, or auto", req.Autonomy)
	}
	if _, ok := s.resolveModel(rt.Model); !ok {
		return workCreated{}, badRequest("unknown model alias %q", rt.Model)
	}
	repos := req.Repositories
	if len(repos) == 0 {
		repos = rt.Repositories
	}
	if len(repos) == 0 {
		return workCreated{}, badRequest("at least one repository is required")
	}
	if len(repos) > protocol.MaxRepositoriesWork {
		return workCreated{}, badRequest("%d repositories exceed %d", len(repos), protocol.MaxRepositoriesWork)
	}
	registered, err := tx.Repositories(ctx)
	if err != nil {
		return workCreated{}, err
	}
	known := make(map[string]bool, len(registered))
	for _, repo := range registered {
		known[repo.Name] = true
	}
	for _, repo := range repos {
		if !known[repo] {
			return workCreated{}, fmt.Errorf("repository %s is not registered: %w", repo, store.ErrNotFound)
		}
	}
	project, err := tx.ProjectForRepository(ctx, repos[0])
	if err != nil {
		return workCreated{}, err
	}
	var edges []model.Edge
	for _, id := range req.After {
		if err := model.ValidateID(id); err != nil {
			return workCreated{}, badRequest("after: %v", err)
		}
		if _, err := tx.GetWork(ctx, id); err != nil {
			return workCreated{}, err
		}
		edges = append(edges, model.Edge{BlockedBy: id, On: model.OnSuccess})
	}
	rt.Repositories = repos
	snapshot, err := json.Marshal(rt)
	if err != nil {
		return workCreated{}, fmt.Errorf("snapshot routine: %w", err)
	}
	w.Title = req.Title
	if w.Title == "" {
		w.Title = titleFromPrompt(rt.Prompt, rt.Name)
	}
	w.Trigger, w.Snapshot, w.Priority, w.BudgetClass = model.TriggerManual, snapshot, rt.Priority, rt.BudgetClass
	w.Autonomy = model.ResolveAutonomy(req.Autonomy, rt.Autonomy, "", project.Autonomy, "")
	w.Integrate, w.Paths, w.SubmittedBy = rt.Integrate, rt.Paths, "human"
	targets, err := tx.CreateWork(ctx, &w, repos, edges)
	if err != nil {
		return workCreated{}, err
	}
	return workCreated{Work: w, Targets: targets}, nil
}

// titleFromPrompt is the prompt's first line, bounded, or the routine's name.
func titleFromPrompt(prompt, fallback string) string {
	line, _, _ := strings.Cut(strings.TrimSpace(prompt), "\n")
	line = strings.TrimSpace(line)
	if line == "" {
		return fallback
	}
	if runes := []rune(line); len(runes) > titleMaxRunes {
		return string(runes[:titleMaxRunes])
	}
	return line
}

// workSummary is one row of GET /api/v1/work.
type workSummary struct {
	Work    store.Work      `json:"work"`
	State   model.WorkState `json:"state"`
	Targets []store.Target  `json:"targets"`
}

func (s *Server) listWork(r *http.Request) (int, any, error) {
	ctx := r.Context()
	limit, err := listLimit(r)
	if err != nil {
		return 0, nil, err
	}
	work, err := s.store.ListWork(ctx, limit)
	if err != nil {
		return 0, nil, err
	}
	ids := make([]string, len(work))
	for i, wk := range work {
		ids[i] = wk.ID
	}
	targets, err := s.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return 0, nil, err
	}
	out := make([]workSummary, 0, len(work))
	for _, wk := range work {
		ts := targets[wk.ID]
		if ts == nil {
			ts = []store.Target{}
		}
		out = append(out, workSummary{Work: wk, State: model.DeriveWorkState(model.WorkInputs{Targets: targetStates(ts), Integrate: wk.Integrate}), Targets: ts})
	}
	return http.StatusOK, out, nil
}

// workDetail is GET /api/v1/work/{id}: the Work, its derived state (without
// dependency or budget effects — the queue shows those), Targets, attempts, and
// questions.
type workDetail struct {
	Work      store.Work       `json:"work"`
	State     model.WorkState  `json:"state"`
	Targets   []store.Target   `json:"targets"`
	Attempts  []store.Attempt  `json:"attempts"`
	Questions []store.Question `json:"questions"`
}

func (s *Server) workDetail(ctx context.Context, id string) (int, any, error) {
	wk, err := s.store.GetWork(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	ts, err := s.store.TargetsForWork(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	ids := make([]string, len(ts))
	for i, t := range ts {
		ids[i] = t.ID
	}
	byTarget, err := s.store.AttemptsForTargets(ctx, ids)
	if err != nil {
		return 0, nil, err
	}
	attempts := []store.Attempt{}
	for _, t := range ts {
		attempts = append(attempts, byTarget[t.ID]...)
	}
	qs, err := s.store.QuestionsForWork(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	if ts == nil {
		ts = []store.Target{}
	}
	if qs == nil {
		qs = []store.Question{}
	}
	state := model.DeriveWorkState(model.WorkInputs{Targets: targetStates(ts), Integrate: wk.Integrate})
	return http.StatusOK, workDetail{Work: *wk, State: state, Targets: ts, Attempts: attempts, Questions: qs}, nil
}

func (s *Server) getWork(r *http.Request) (int, any, error) {
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	return s.workDetail(r.Context(), id)
}

// cancelWork is exempt from the drain refusal: cancelling hastens a drain.
func (s *Server) cancelWork(r *http.Request) (int, any, error) {
	ctx := r.Context()
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.CancelWork(ctx, id, "human") }); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "work cancelled", "work_id", id)
	return s.workDetail(ctx, id)
}

// workPatch is PATCH /api/v1/work/{id}: the queue's reorder and dependency edits.
type workPatch struct {
	Priority        *int         `json:"priority"`
	AddBlockedBy    []dependency `json:"add_blocked_by"`
	RemoveBlockedBy []string     `json:"remove_blocked_by"`
	// MoveBefore reorders: place this Work immediately above the named one ("" =
	// the queue's tail). The daemon refuses an order that would put a Work above
	// one it is blocked by — the one rule for the CLI and the UI drag alike.
	MoveBefore *string `json:"move_before"`
}

type dependency struct {
	WorkID string             `json:"work_id"`
	On     model.DependencyOn `json:"on"`
}

func (s *Server) patchWork(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	var patch workPatch
	if err := decodeJSON(r, &patch); err != nil {
		return 0, nil, err
	}
	for i, d := range patch.AddBlockedBy {
		if err := model.ValidateID(d.WorkID); err != nil {
			return 0, nil, badRequest("add_blocked_by: %v", err)
		}
		switch d.On {
		case "":
			patch.AddBlockedBy[i].On = model.OnSuccess
		case model.OnSuccess, model.OnTerminal:
		default:
			return 0, nil, badRequest("add_blocked_by: on %q: want success or terminal", d.On)
		}
	}
	for _, dep := range patch.RemoveBlockedBy {
		if err := model.ValidateID(dep); err != nil {
			return 0, nil, badRequest("remove_blocked_by: %v", err)
		}
	}
	if patch.MoveBefore != nil {
		status, body, err := s.moveWork(ctx, id, *patch.MoveBefore)
		if err != nil || patch.Priority == nil && len(patch.AddBlockedBy) == 0 && len(patch.RemoveBlockedBy) == 0 {
			return status, body, err
		}
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		if patch.Priority != nil {
			if err := tx.SetPriority(ctx, id, *patch.Priority); err != nil {
				return err
			}
		}
		for _, d := range patch.AddBlockedBy {
			if _, err := tx.GetWork(ctx, d.WorkID); err != nil {
				return err
			}
			if err := tx.AddDependency(ctx, model.Edge{Work: id, BlockedBy: d.WorkID, On: d.On}); err != nil {
				return err
			}
		}
		for _, dep := range patch.RemoveBlockedBy {
			if err := tx.RemoveDependency(ctx, id, dep); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "work patched", "work_id", id, "priority", patch.Priority != nil, "added", len(patch.AddBlockedBy), "removed", len(patch.RemoveBlockedBy))
	return s.workDetail(ctx, id)
}

// moveWork places one Work immediately above another (or at the tail),
// refusing an order that puts it above a dependency (queue.Violates — the same
// rule the drag UI relies on).
func (s *Server) moveWork(ctx context.Context, id, before string) (int, any, error) {
	if before != "" {
		if err := model.ValidateID(before); err != nil {
			return 0, nil, badRequest("move_before: %v", err)
		}
	}
	order, err := s.loadQueue(ctx)
	if err != nil {
		return 0, nil, err
	}
	inQueue := false
	for _, e := range order {
		if e.Work.ID == id {
			inQueue = true
		}
	}
	if !inQueue {
		return 0, nil, fmt.Errorf("work %s: %w", id, store.ErrNotFound)
	}
	edges, err := s.store.DependencyEdges(ctx)
	if err != nil {
		return 0, nil, err
	}
	if before != "" && Violates(order, edges, id, before) {
		return 0, nil, fmt.Errorf("moving %s above %s would put it before a task it is blocked by: %w", id[:8], before[:8], store.ErrConflict)
	}
	priority := 0
	found := false
	if before == "" {
		for _, e := range order {
			if e.Work.ID == id {
				found = true
			}
			if e.Work.Priority-1 < priority || priority == 0 {
				priority = e.Work.Priority - 1
			}
		}
	} else {
		for _, e := range order {
			if e.Work.ID == id {
				found = true
			}
			if e.Work.ID == before {
				priority = e.Work.Priority + 1
			}
		}
	}
	if !found {
		return 0, nil, fmt.Errorf("work %s: %w", id, store.ErrNotFound)
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error { return tx.SetPriority(ctx, id, priority) })
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "work moved", "work_id", id, "before", before, "priority", priority)
	return s.workDetail(ctx, id)
}

// queueItem is one row of GET /api/v1/queue.
type queueItem struct {
	Work     store.Work      `json:"work"`
	State    model.WorkState `json:"state"`
	Reason   string          `json:"reason,omitempty"`
	Waiting  []string        `json:"waiting,omitempty"`
	FailedOn []string        `json:"failed_on,omitempty"`
	Position int             `json:"position"`
	Targets  []store.Target  `json:"targets"`
}

// loadQueue is the read-only twin of claimTx's view, for display.
func (s *Server) loadQueue(ctx context.Context) ([]QueueEntry, error) {
	work, err := s.store.OpenWork(ctx)
	if err != nil {
		return nil, err
	}
	ids := make([]string, len(work))
	for i, wk := range work {
		ids[i] = wk.ID
	}
	targets, err := s.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, err
	}
	edges, err := s.store.DependencyEdges(ctx)
	if err != nil {
		return nil, err
	}
	finished, err := s.finishedStates(ctx, work, edges)
	if err != nil {
		return nil, err
	}
	return Order(QueueInput{Work: work, Targets: targets, Edges: edges, Deferred: s.deferred, FinishedStates: finished}), nil
}

func (s *Server) queue(r *http.Request) (int, any, error) {
	order, err := s.loadQueue(r.Context())
	if err != nil {
		return 0, nil, err
	}
	out := make([]queueItem, 0, len(order))
	for _, e := range order {
		ts := e.Targets
		if ts == nil {
			ts = []store.Target{}
		}
		out = append(out, queueItem{Work: e.Work, State: e.State, Reason: e.Reason, Waiting: e.Waiting, FailedOn: e.FailedOn, Position: e.Position, Targets: ts})
	}
	return http.StatusOK, out, nil
}

// answerRequest is POST /api/v1/questions/{id}/answer.
type answerRequest struct {
	Answer string `json:"answer"`
	By     string `json:"by"`
}

func (s *Server) answer(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	var req answerRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	if strings.TrimSpace(req.Answer) == "" {
		return 0, nil, badRequest("answer is required")
	}
	if req.By == "" {
		req.By = "human"
	}
	var q *store.Question
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		q, err = tx.AnswerQuestion(ctx, id, req.Answer, req.By)
		return err
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "question answered", "question_id", q.ID, "attempt_id", q.AttemptID, "target_id", q.TargetID, "by", req.By)
	return http.StatusOK, q, nil
}

// attemptDetail is GET /api/v1/attempts/{id}; facts is null until the attempt
// is terminal.
type attemptDetail struct {
	Attempt store.Attempt       `json:"attempt"`
	Facts   *store.AttemptFacts `json:"facts"`
}

func (s *Server) getAttempt(r *http.Request) (int, any, error) {
	ctx := r.Context()
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	a, err := s.store.GetAttempt(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	facts, err := s.store.FactsForAttempt(ctx, id)
	if err != nil && !errors.Is(err, store.ErrNotFound) {
		return 0, nil, err
	}
	return http.StatusOK, attemptDetail{Attempt: *a, Facts: facts}, nil
}

func (s *Server) getEvents(r *http.Request) (int, any, error) {
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	limit, err := listLimit(r)
	if err != nil {
		return 0, nil, err
	}
	lines := r.URL.Query().Get("lines") != "false"
	events, err := s.store.Events(r.Context(), id, lines, limit)
	if err != nil {
		return 0, nil, err
	}
	if events == nil {
		events = []store.StoredEvent{}
	}
	return http.StatusOK, events, nil
}

func (s *Server) workers(r *http.Request) (int, any, error) {
	workers, err := s.store.Workers(r.Context(), s.now().UTC())
	if err != nil {
		return 0, nil, err
	}
	if workers == nil {
		workers = []store.Worker{}
	}
	return http.StatusOK, workers, nil
}

func (s *Server) repositories(r *http.Request) (int, any, error) {
	repos, err := s.store.Repositories(r.Context())
	if err != nil {
		return 0, nil, err
	}
	if repos == nil {
		repos = []store.Repository{}
	}
	return http.StatusOK, repos, nil
}

// attention is GET /api/v1/attention: what needs a human — open questions and
// undecided proposals (M3 adds conflicts and L3 approvals).
type attention struct {
	Questions []store.Question `json:"questions"`
	Proposals []store.Proposal `json:"proposals"`
}

func (s *Server) attention(r *http.Request) (int, any, error) {
	qs, err := s.store.OpenQuestions(r.Context())
	if err != nil {
		return 0, nil, err
	}
	if qs == nil {
		qs = []store.Question{}
	}
	ps, err := s.store.ListProposals(r.Context(), model.ProposalProposed)
	if err != nil {
		return 0, nil, err
	}
	if ps == nil {
		ps = []store.Proposal{}
	}
	return http.StatusOK, attention{Questions: qs, Proposals: ps}, nil
}
