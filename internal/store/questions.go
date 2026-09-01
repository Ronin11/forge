package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"forge/internal/core/model"
	"forge/internal/protocol"
)

// Question is raised by an attempt and pauses its Target.
type Question struct {
	ID          string          `json:"id"`
	AttemptID   string          `json:"attempt_id"`
	TargetID    string          `json:"target_id"`
	WorkID      string          `json:"work_id"`
	Text        string          `json:"text"`
	Options     []string        `json:"options,omitempty"`
	Context     json.RawMessage `json:"context,omitempty"`
	Checkpoint  string          `json:"checkpoint,omitempty"`
	Criticality string          `json:"criticality"`
	Answer      string          `json:"answer,omitempty"`
	AnsweredBy  string          `json:"answered_by,omitempty"`
	Rationale   string          `json:"rationale,omitempty"`
	AskedAt     time.Time       `json:"asked_at"`
	AnsweredAt  time.Time       `json:"answered_at,omitempty"`
}

// IsAuto reports whether the answer came from the attention sweep's decider
// rather than a human (AnsweredBy "auto:<model>").
func (q Question) IsAuto() bool { return strings.HasPrefix(q.AnsweredBy, "auto:") }

// AutoModel is the decider model behind an auto-answer ("auto:opus" → "opus");
// empty for a human answer.
func (q Question) AutoModel() string {
	if !q.IsAuto() {
		return ""
	}
	return strings.TrimPrefix(q.AnsweredBy, "auto:")
}

// CreateQuestion records a question for an attempt.
func (tx *Tx) CreateQuestion(ctx context.Context, a *Attempt, q protocol.QuestionRequest) (*Question, error) {
	t, err := tx.GetTarget(ctx, a.TargetID)
	if err != nil {
		return nil, err
	}
	crit := model.Criticality(q.Criticality)
	if crit == "" {
		crit = model.CriticalityNormal
	}
	if !crit.Valid() {
		return nil, fmt.Errorf("question criticality %q: want critical|normal|low", q.Criticality)
	}
	out := &Question{ID: model.NewID(), AttemptID: a.ID, TargetID: a.TargetID, WorkID: t.WorkID, Text: q.Text, Options: q.Options, Context: q.Context, Checkpoint: q.Checkpoint, Criticality: string(crit), AskedAt: tx.now}
	if _, err := tx.Exec(ctx, `INSERT INTO questions (id, attempt_id, target_id, work_id, text, options, context, checkpoint, criticality, asked_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		out.ID, out.AttemptID, out.TargetID, out.WorkID, out.Text, jsonOrNull(out.Options), jsonRaw(out.Context), nullString(out.Checkpoint), out.Criticality, formatTime(tx.now)); err != nil {
		return nil, fmt.Errorf("insert question: %w", err)
	}
	if err := tx.Journal(ctx, "question.asked", EntityQuestion, out.ID, map[string]any{"attempt_id": a.ID, "target_id": a.TargetID, "checkpoint": q.Checkpoint, "criticality": out.Criticality}); err != nil {
		return nil, err
	}
	return out, nil
}

// AnswerQuestion records a human answer and re-queues the Target (pinned to its
// worker) so the worker resumes the session.
func (tx *Tx) AnswerQuestion(ctx context.Context, questionID, answer, by string) (*Question, error) {
	return tx.answerQuestion(ctx, questionID, answer, by, "", "question.answered", nil)
}

// AutoAnswerQuestion records the attention sweep's decision (DESIGN.md §10.4):
// the decider model's answer plus its rationale, journalled distinctly as
// question.auto_answered with the criticality and the lapsed deadline so the
// auto path is unmistakable in the audit. by is "auto:<model>".
func (tx *Tx) AutoAnswerQuestion(ctx context.Context, questionID, answer, by, rationale string, deadline time.Time) (*Question, error) {
	return tx.answerQuestion(ctx, questionID, answer, by, rationale, "question.auto_answered",
		map[string]any{"deadline": formatTime(deadline.UTC())})
}

// answerQuestion is the shared core for the human and auto answer paths; extra
// is merged into the journal attrs and, for the auto path, the criticality,
// answer, and rationale are added so the audit is complete.
func (tx *Tx) answerQuestion(ctx context.Context, questionID, answer, by, rationale, kind string, extra map[string]any) (*Question, error) {
	qs, err := scanQuestions(each(tx.Query(ctx, `SELECT `+questionColumns+` FROM questions WHERE id = ?`, questionID)))
	if err != nil {
		return nil, err
	}
	if len(qs) == 0 {
		return nil, fmt.Errorf("question %s: %w", questionID, ErrNotFound)
	}
	q := qs[0]
	if !q.AnsweredAt.IsZero() {
		return nil, fmt.Errorf("question %s already answered: %w", questionID, ErrConflict)
	}
	q.Answer, q.AnsweredBy, q.Rationale, q.AnsweredAt = answer, by, rationale, tx.now
	if _, err := tx.Exec(ctx, `UPDATE questions SET answer = ?, answered_by = ?, rationale = ?, answered_at = ? WHERE id = ?`, answer, by, nullString(rationale), formatTime(tx.now), questionID); err != nil {
		return nil, fmt.Errorf("answer question %s: %w", questionID, err)
	}
	attrs := map[string]any{"by": by, "target_id": q.TargetID}
	if kind == "question.auto_answered" {
		attrs["criticality"], attrs["answer"], attrs["rationale"] = q.Criticality, answer, rationale
	}
	for k, v := range extra {
		attrs[k] = v
	}
	if err := tx.Journal(ctx, kind, EntityQuestion, questionID, attrs); err != nil {
		return nil, err
	}
	var open int
	if err := tx.QueryRow(ctx, `SELECT count(*) FROM questions WHERE attempt_id = ? AND answered_at IS NULL`, q.AttemptID).Scan(&open); err != nil {
		return nil, fmt.Errorf("count open questions: %w", err)
	}
	if open == 0 {
		// Only resume a target still parked on the question. If it reached a
		// terminal state (e.g. cancelled) while the question sat open,
		// answering the stale question must not resurrect it — cancelled →
		// pending is a legal edge (it exists for retry, see retry.go), so a
		// bare Transition would silently reopen a finished Work.
		t, err := tx.GetTarget(ctx, q.TargetID)
		if err != nil {
			return nil, err
		}
		if t.State == model.WaitingHuman {
			if _, err := tx.Transition(ctx, q.TargetID, model.Pending, TransitionOptions{Actor: by}); err != nil {
				return nil, err
			}
		}
	}
	return &q, nil
}

const questionColumns = `id, attempt_id, target_id, work_id, text, options, context, checkpoint, criticality, answer, answered_by, rationale, asked_at, answered_at`

// OpenQuestions lists unanswered questions, oldest first.
func (s *Store) OpenQuestions(ctx context.Context) ([]Question, error) {
	return scanQuestions(each(s.query(ctx, `SELECT `+questionColumns+` FROM questions WHERE answered_at IS NULL ORDER BY asked_at`)))
}

// QuestionsForWork lists a Work's questions, oldest first.
func (s *Store) QuestionsForWork(ctx context.Context, workID string) ([]Question, error) {
	return scanQuestions(each(s.query(ctx, `SELECT `+questionColumns+` FROM questions WHERE work_id = ? ORDER BY asked_at`, workID)))
}

// LastAnswer returns the latest answered question of an attempt (what a resume
// sends as the prompt), or nil.
func (tx *Tx) LastAnswer(ctx context.Context, attemptID string) (*Question, error) {
	qs, err := scanQuestions(each(tx.Query(ctx, `SELECT `+questionColumns+` FROM questions WHERE attempt_id = ? AND answered_at IS NOT NULL ORDER BY answered_at DESC LIMIT 1`, attemptID)))
	if err != nil {
		return nil, err
	}
	if len(qs) == 0 {
		return nil, nil
	}
	return &qs[0], nil
}

// OpenQuestionsForAttempt counts unanswered questions of an attempt.
func (tx *Tx) OpenQuestionsForAttempt(ctx context.Context, attemptID string) (int, error) {
	var n int
	if err := tx.QueryRow(ctx, `SELECT count(*) FROM questions WHERE attempt_id = ? AND answered_at IS NULL`, attemptID).Scan(&n); err != nil {
		return 0, fmt.Errorf("count open questions of %s: %w", attemptID, err)
	}
	return n, nil
}

func scanQuestions(iter func(func(*sql.Rows) error) error) ([]Question, error) {
	var out []Question
	err := iter(func(rows *sql.Rows) error {
		var q Question
		var options, context, checkpoint, answer, by, rationale, answeredAt sql.NullString
		var asked string
		if err := rows.Scan(&q.ID, &q.AttemptID, &q.TargetID, &q.WorkID, &q.Text, &options, &context, &checkpoint, &q.Criticality, &answer, &by, &rationale, &asked, &answeredAt); err != nil {
			return fmt.Errorf("scan question: %w", err)
		}
		q.Checkpoint, q.Answer, q.AnsweredBy, q.Rationale = checkpoint.String, answer.String, by.String, rationale.String
		if context.Valid {
			q.Context = json.RawMessage(context.String)
		}
		var err error
		if q.Options, err = jsonStrings(options); err != nil {
			return err
		}
		if q.AskedAt, err = parseTime(sql.NullString{String: asked, Valid: true}); err != nil {
			return err
		}
		if q.AnsweredAt, err = parseTime(answeredAt); err != nil {
			return err
		}
		out = append(out, q)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read questions: %w", err)
	}
	return out, nil
}
