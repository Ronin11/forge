package controlplane

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"

	"forge/internal/model"
	"forge/internal/modes"
	"forge/internal/protocol"
	"forge/internal/store"
)

// Follow-up Work defaults where a WorkSpec leaves fields unset; the verify
// prompt is small and the budget deliberately modest.
const (
	followUpModel    = "haiku"
	followUpExecutor = "claude-code"
	followUpTimeout  = 1800
	followUpMaxTurns = 30
)

// verifyRoutes serves the L2/L3 verification surface: the human decision on a
// verifying Target and the verifications rows behind it. The attention list
// (handlers_operator.go) still shows only questions; a Target waiting for L3
// is found via `forge task show` or GET /api/v1/verifications.
func (s *Server) verifyRoutes(m *http.ServeMux) {
	m.HandleFunc("POST /api/v1/targets/{id}/approve", s.handle(s.approveTarget))
	m.HandleFunc("POST /api/v1/targets/{id}/reject", s.handle(s.rejectTarget))
	// M11 retry and steer live beside the other POST decisions; the handlers
	// are in handlers_operator.go.
	m.HandleFunc("POST /api/v1/targets/{id}/retry", s.handle(s.retryTarget))
	m.HandleFunc("POST /api/v1/targets/{id}/reverify", s.handle(s.reverifyTarget))
	m.HandleFunc("POST /api/v1/targets/{id}/requeue", s.handle(s.requeueTarget))
	m.HandleFunc("POST /api/v1/attempts/{id}/steer", s.handle(s.steerAttempt))
	m.HandleFunc("GET /api/v1/verifications", s.handle(s.listVerifications))
}

// decisionRequest is the body of approve and reject.
type decisionRequest struct {
	By     string `json:"by"`
	Reason string `json:"reason"`
}

func (s *Server) approveTarget(r *http.Request) (int, any, error) { return s.decideTarget(r, true) }
func (s *Server) rejectTarget(r *http.Request) (int, any, error)  { return s.decideTarget(r, false) }

// decideTarget is L3 (VERIFICATION.md): a human moves a verifying Target to
// succeeded or unverified (human_rejected); the decision is a verifications
// row at level 3 on the Target's latest attempt.
func (s *Server) decideTarget(r *http.Request, approve bool) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	var req decisionRequest
	if r.ContentLength != 0 {
		if err := decodeJSON(r, &req); err != nil {
			return 0, nil, err
		}
	}
	if req.By == "" {
		req.By = "human"
	}
	var target *store.Target
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		t, err := tx.GetTarget(ctx, id)
		if err != nil {
			return err
		}
		if t.State != model.Verifying {
			return fmt.Errorf("target %s is %s, not verifying: %w", id, t.State, store.ErrConflict)
		}
		a, err := s.store.AttemptForTarget(ctx, t.ID)
		if err != nil {
			return err
		}
		if a == nil {
			return fmt.Errorf("target %s has no attempt: %w", id, store.ErrConflict)
		}
		verdict, err := json.Marshal(map[string]string{"by": req.By, "reason": req.Reason})
		if err != nil {
			return fmt.Errorf("encode decision: %w", err)
		}
		if err := tx.RecordVerification(ctx, a.ID, int(model.L3), approve, "", req.By, verdict); err != nil {
			return err
		}
		if approve {
			target, err = tx.Transition(ctx, t.ID, model.Succeeded, store.TransitionOptions{Actor: req.By})
		} else {
			target, err = tx.Transition(ctx, t.ID, model.Unverified, store.TransitionOptions{UnverifiedReason: "human_rejected", Actor: req.By})
		}
		if err != nil {
			return err
		}
		if err := s.recordFacts(ctx, tx, a.ID); err != nil {
			return err
		}
		tw, err := tx.GetWork(ctx, t.WorkID)
		if err != nil {
			return err
		}
		if err := s.enqueueMerge(ctx, tx, tw, t.ID); err != nil {
			return err
		}
		target, err = tx.GetTarget(ctx, t.ID)
		return err
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "target decided", "target_id", id, "approved", approve, "by", req.By, "state", target.State)
	return http.StatusOK, target, nil
}

// reverifyTarget is `forge task reverify`: an unverified Target goes back to
// verifying and a fresh verify follow-up is created from its stored result
// envelope — the completed work is re-checked without re-running the subject.
// The recovery for verify_attempt_failed (the verifier, not the subject, ran
// out of budget), and a second opinion on any other unverified outcome.
func (s *Server) reverifyTarget(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	if s.modes == nil {
		return 0, nil, fmt.Errorf("no mode registry: %w", store.ErrConflict)
	}
	var target *store.Target
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		t, err := tx.GetTarget(ctx, id)
		if err != nil {
			return err
		}
		if t.State != model.Unverified {
			return fmt.Errorf("target %s is %s, not unverified: %w", id, t.State, store.ErrConflict)
		}
		a, err := s.store.AttemptForTarget(ctx, t.ID)
		if err != nil {
			return err
		}
		if a == nil {
			return fmt.Errorf("target %s has no attempt: %w", id, store.ErrConflict)
		}
		mode := s.modes.Get(a.Mode)
		if mode == nil {
			return fmt.Errorf("mode %q unknown: %w", a.Mode, store.ErrConflict)
		}
		env := decodeEnvelope(a.Result)
		if env == nil {
			return fmt.Errorf("attempt %s left no parseable result envelope to verify: %w", a.ID, store.ErrConflict)
		}
		w, err := tx.GetWork(ctx, t.WorkID)
		if err != nil {
			return err
		}
		ft := s.repoForgeToml(ctx, tx, t.Repository)
		fc := modes.FollowUpContext{
			AttemptID: a.ID, TargetID: t.ID, WorkID: w.ID, Repository: t.Repository,
			Branch: a.Branch, Head: a.HeadCommit, Class: w.BudgetClass, Priority: w.Priority, UI: ft.Verify.UI,
		}
		var specs []modes.WorkSpec
		for _, spec := range mode.FollowUps(env, fc) {
			if spec.VerifyOf != nil {
				specs = append(specs, spec)
			}
		}
		if len(specs) == 0 {
			return fmt.Errorf("mode %q produces no verify follow-up: %w", a.Mode, store.ErrConflict)
		}
		if target, err = tx.ReverifyTarget(ctx, t.ID); err != nil {
			return err
		}
		for _, spec := range specs {
			if err := s.createFollowUp(ctx, tx, w, spec); err != nil {
				return fmt.Errorf("verify follow-up of %s: %w", a.ID, err)
			}
		}
		return nil
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "target reverified", "target_id", id)
	return http.StatusOK, target, nil
}

// listVerifications is GET /api/v1/verifications?attempt_id=…: the rows of one
// subject attempt.
func (s *Server) listVerifications(r *http.Request) (int, any, error) {
	id := r.URL.Query().Get("attempt_id")
	if err := model.ValidateID(id); err != nil {
		return 0, nil, badRequest("attempt_id: %v", err)
	}
	rows, err := s.store.VerificationsForAttempt(r.Context(), id)
	if err != nil {
		return 0, nil, err
	}
	if rows == nil {
		rows = []store.Verification{}
	}
	return http.StatusOK, rows, nil
}

// verifyOfFromSnapshot decodes the subject link a verify Work carries in its
// snapshot under "verify_of"; nil for ordinary Work. The claim builder attaches
// it to the Claim so the worker cuts the worktree at the subject's head.
func verifyOfFromSnapshot(snap json.RawMessage) *protocol.VerifyOf {
	if len(snap) == 0 {
		return nil
	}
	var v struct {
		VerifyOf *protocol.VerifyOf `json:"verify_of"`
	}
	if err := json.Unmarshal(snap, &v); err != nil {
		return nil
	}
	return v.VerifyOf
}

// repoForgeToml is the lenient view of repositories.forge_toml (the worker's
// JSON-encoded worker.ForgeToml, whose fields carry no tags) that verification
// needs: [verify] ui and [modes.<name>] paths. Undecodable columns are treated
// as absent, never fatal.
type repoForgeToml struct {
	Verify struct {
		UI bool
	}
	Modes map[string]struct {
		Paths []string
	}
}

func (s *Server) repoForgeToml(ctx context.Context, tx *store.Tx, repository string) repoForgeToml {
	var out repoForgeToml
	repos, err := tx.Repositories(ctx)
	if err != nil {
		s.log.WarnContext(ctx, "read repositories for forge.toml", "error", err)
		return out
	}
	for _, r := range repos {
		if r.Name != repository {
			continue
		}
		if r.ForgeToml != "" {
			if err := json.Unmarshal([]byte(r.ForgeToml), &out); err != nil {
				s.log.DebugContext(ctx, "forge_toml column undecodable; treated as absent", "repository", repository, "error", err)
			}
		}
		return out
	}
	return out
}

// claimModeInfo renders the mode's verification contract for a claim; a nil
// registry or an unknown mode is the M1 default: repo writes, level 1.
func (s *Server) claimModeInfo(ctx context.Context, tx *store.Tx, mode, repository string) *protocol.ModeInfo {
	mi := &protocol.ModeInfo{WriteScope: model.WritesRepo, RequiredLevel: 1}
	if s.modes != nil {
		if m := s.modes.Get(mode); m != nil {
			mi.WriteScope, mi.RequiredLevel, mi.Checkpoints = m.Writes(), int(m.Level()), m.Checkpoints()
			mi.Schema = m.ResultSchema()
		}
	}
	ft := s.repoForgeToml(ctx, tx, repository)
	if docs, ok := ft.Modes["docs"]; ok {
		mi.DocsPaths = docs.Paths
	}
	return mi
}

// modeRequiredLevel is what tx.Complete gates the worker's L0/L1 outcome
// against: the registered mode's Level(), or the configured fallback (1) when
// no registry is wired.
func (s *Server) modeRequiredLevel(mode string) int {
	if s.modes != nil {
		if m := s.modes.Get(mode); m != nil {
			return int(m.Level())
		}
	}
	return s.requiredLevel(mode)
}

// envelopeKeys are the common result fields; everything else in a raw result
// is the mode's Extra.
var envelopeKeys = [...]string{"schema_version", "summary", "needs_input", "changes", "checks_run", "claims"}

// decodeEnvelope re-reads a raw result as the envelope plus mode extras; nil
// when there is none or it does not parse (the worker already classified that).
func decodeEnvelope(raw json.RawMessage) *protocol.ResultEnvelope {
	if len(raw) == 0 {
		return nil
	}
	var env protocol.ResultEnvelope
	if err := json.Unmarshal(raw, &env); err != nil {
		return nil
	}
	var extra map[string]json.RawMessage
	if err := json.Unmarshal(raw, &extra); err == nil {
		for _, k := range envelopeKeys {
			delete(extra, k)
		}
		env.Extra = extra
	}
	return &env
}

// afterComplete is the M4 hook inside the complete transaction: it records the
// worker's verification row and any artifacts, applies a verify attempt's
// verdict to its subject, and asks the mode for follow-up Work when the Target
// stays in verifying (VERIFICATION.md L2/L3). Never called for late or
// idempotent-retry completions.
func (s *Server) afterComplete(ctx context.Context, tx *store.Tx, a *store.Attempt, w *store.Work, t *store.Target, req protocol.CompleteRequest) error {
	if len(req.Artifacts) > 0 {
		if err := tx.RecordArtifacts(ctx, a.ID, req.Artifacts); err != nil {
			return err
		}
	}
	if req.State == model.Succeeded {
		if err := tx.RecordVerification(ctx, a.ID, req.Verification.Level, req.Verification.Passed, "", "worker", req.Verification.Verdict); err != nil {
			return err
		}
	}
	if vo := verifyOfFromSnapshot(w.Snapshot); vo != nil {
		return s.applyVerifyVerdict(ctx, tx, a, t, vo, req)
	}
	if t.State == model.Verifying && req.State == model.Succeeded && req.Verification.Passed && s.modes != nil {
		return s.verifyFollowUps(ctx, tx, a, w, t, req)
	}
	if a.Mode == "plan" && t.State == model.Succeeded && req.Verification.Passed {
		// The batch a plan produces (DESIGN.md §20) is created here, in the
		// completion transaction, so the tasks appear together or not at all.
		return s.planFollowUps(ctx, tx, a, w, t, decodeEnvelope(req.Result))
	}
	return nil
}

// enqueueMerge moves a succeeded Target of an integrating Work into the merge
// queue (DESIGN.md §4.1: succeeded → queued_for_merge, only with integrate).
// Safe to call for any Target; anything not integrating-and-succeeded is left
// alone.
func (s *Server) enqueueMerge(ctx context.Context, tx *store.Tx, w *store.Work, targetID string) error {
	if w == nil || !w.Integrate {
		return nil
	}
	t, err := tx.GetTarget(ctx, targetID)
	if err != nil {
		return err
	}
	if t.State != model.Succeeded {
		return nil
	}
	if _, err := tx.Transition(ctx, t.ID, model.QueuedForMerge, store.TransitionOptions{Actor: "daemon"}); err != nil {
		return err
	}
	s.log.InfoContext(ctx, "queued for merge", "target_id", t.ID, "work_id", w.ID, "repository", t.Repository)
	return nil
}

// applyVerifyVerdict decides the SUBJECT of a verify attempt (§4.1): verdict
// pass → succeeded (unless the mode still requires L3), fail or inconclusive →
// unverified with the verdict, and a verify attempt that itself failed or
// produced no verdict → unverified verify_attempt_failed.
func (s *Server) applyVerifyVerdict(ctx context.Context, tx *store.Tx, verifier *store.Attempt, verifierTarget *store.Target, vo *protocol.VerifyOf, req protocol.CompleteRequest) error {
	if verifierTarget.State == model.Verifying || verifierTarget.State == model.WaitingHuman {
		// The verify attempt is not decided yet; the subject keeps waiting.
		return nil
	}
	subject, err := tx.GetAttempt(ctx, vo.AttemptID)
	if err != nil {
		return err
	}
	st, err := tx.GetTarget(ctx, subject.TargetID)
	if err != nil {
		return err
	}
	if st.State != model.Verifying {
		// Already decided (a human, a sweep, a racing verdict); record nothing.
		return tx.Journal(ctx, "verify.verdict_ignored", store.EntityTarget, st.ID, map[string]any{"state": st.State, "verifier_attempt_id": verifier.ID})
	}
	verdict := ""
	if model.IsSuccess(verifierTarget.State) && len(req.Result) > 0 {
		var extra struct {
			Verdict string `json:"verdict"`
		}
		if err := json.Unmarshal(req.Result, &extra); err == nil {
			verdict = extra.Verdict
		}
	}
	passed, reason := false, ""
	switch verdict {
	case "pass":
		passed = true
	case "fail", "inconclusive":
		reason = "verify_verdict:" + verdict
	default:
		reason = "verify_attempt_failed"
	}
	detail, err := json.Marshal(map[string]any{"verdict": verdict, "verifier_state": verifierTarget.State, "reason": reason})
	if err != nil {
		return fmt.Errorf("encode verdict: %w", err)
	}
	if err := tx.RecordVerification(ctx, subject.ID, int(model.L2), passed, verifier.ID, "verify", detail); err != nil {
		return err
	}
	if passed && s.modeRequiredLevel(subject.Mode) >= int(model.L3) {
		// L3 still to come: the subject waits in verifying for the human.
		return nil
	}
	if passed {
		_, err = tx.Transition(ctx, st.ID, model.Succeeded, store.TransitionOptions{Actor: "daemon"})
	} else {
		_, err = tx.Transition(ctx, st.ID, model.Unverified, store.TransitionOptions{UnverifiedReason: reason, Actor: "daemon"})
	}
	if err != nil {
		return err
	}
	if err := s.recordFacts(ctx, tx, subject.ID); err != nil {
		return err
	}
	sw, err := tx.GetWork(ctx, st.WorkID)
	if err != nil {
		return err
	}
	return s.enqueueMerge(ctx, tx, sw, st.ID)
}

// verifyFollowUps hands the successful result to the mode and creates each
// WorkSpec it returns in the same transaction.
func (s *Server) verifyFollowUps(ctx context.Context, tx *store.Tx, a *store.Attempt, w *store.Work, t *store.Target, req protocol.CompleteRequest) error {
	mode := s.modes.Get(a.Mode)
	if mode == nil {
		return nil
	}
	env := decodeEnvelope(req.Result)
	if env == nil {
		return nil
	}
	ft := s.repoForgeToml(ctx, tx, t.Repository)
	fc := modes.FollowUpContext{
		AttemptID: a.ID, TargetID: t.ID, WorkID: w.ID, Repository: t.Repository,
		Branch: a.Branch, Head: req.Git.Head, Class: w.BudgetClass, Priority: w.Priority, UI: ft.Verify.UI,
	}
	for _, spec := range mode.FollowUps(env, fc) {
		if err := s.createFollowUp(ctx, tx, w, spec); err != nil {
			return fmt.Errorf("follow-up of %s: %w", a.ID, err)
		}
	}
	return nil
}

// createFollowUp freezes one WorkSpec as routine-less Work (routine_id NULL,
// generation 0, trigger dependency) with a Routine-shaped snapshot; a VerifyOf
// spec carries the subject link in the snapshot under "verify_of", which the
// claim builder reads back with verifyOfFromSnapshot. subject is the Work whose
// run produced this follow-up: it becomes the caused_by parent so the audit
// link is a first-class column, not only attempt-granular snapshot JSON.
func (s *Server) createFollowUp(ctx context.Context, tx *store.Tx, subject *store.Work, spec modes.WorkSpec) error {
	if err := model.ValidateName(spec.Mode); err != nil {
		return fmt.Errorf("mode: %w", err)
	}
	if spec.Repository == "" {
		return fmt.Errorf("workspec %s: repository is required", spec.Mode)
	}
	class := spec.Class
	if class == "" {
		class = model.ClassNormal
	}
	autonomy := spec.Autonomy
	if autonomy == "" {
		autonomy = model.AutonomyAuto
	}
	timeout := spec.Timeout
	if timeout <= 0 {
		timeout = followUpTimeout
	}
	maxTurns := spec.MaxTurns
	if maxTurns <= 0 {
		maxTurns = followUpMaxTurns
	}
	rt := store.Routine{
		Name: spec.Mode, Mode: spec.Mode, Prompt: spec.Prompt, Repositories: []string{spec.Repository},
		Executor: followUpExecutor, Model: followUpModel, MaxTurns: maxTurns, TimeoutSeconds: timeout,
		Autonomy: autonomy, Priority: spec.Priority, BudgetClass: class, Concurrency: 1,
	}
	var verifyOf *protocol.VerifyOf
	if v := spec.VerifyOf; v != nil {
		verifyOf = &protocol.VerifyOf{AttemptID: v.AttemptID, Branch: v.Branch, Head: v.Head, UI: v.UI}
	}
	snap, err := json.Marshal(struct {
		store.Routine
		VerifyOf *protocol.VerifyOf `json:"verify_of,omitempty"`
	}{Routine: rt, VerifyOf: verifyOf})
	if err != nil {
		return fmt.Errorf("snapshot: %w", err)
	}
	title := spec.Title
	if title == "" {
		title = titleFromPrompt(spec.Prompt, spec.Mode)
	}
	cause := model.CauseFollowUp
	if verifyOf != nil {
		cause = model.CauseVerify
	}
	work := &store.Work{
		RoutineName: spec.Mode, Title: title, Trigger: model.TriggerDependency, Snapshot: snap,
		Priority: spec.Priority, BudgetClass: class, Autonomy: autonomy, SubmittedBy: "daemon",
		CausedByWorkID: subject.ID, Cause: cause,
	}
	if _, err := tx.CreateWork(ctx, work, []string{spec.Repository}, nil); err != nil {
		return err
	}
	s.log.InfoContext(ctx, "follow-up work created", "work_id", work.ID, "mode", spec.Mode, "repository", spec.Repository, "verify_of", verifyOf != nil)
	return nil
}
