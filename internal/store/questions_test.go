package store

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/protocol"
)

// TestQuestionCriticality covers the agent-declared criticality: an empty value
// defaults to normal, a known value is stored, and an unknown value is rejected
// at CreateQuestion.
func TestQuestionCriticality(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	a := f.claim(target, "r1")
	f.run(a.ID, "lease-r1")

	// Default: empty → normal, recorded on the row and in the journal.
	var q *Question
	f.write(func(tx *Tx) error {
		var err error
		q, err = tx.CreateQuestion(ctx(), a, protocol.QuestionRequest{Text: "which?"})
		return err
	})
	if q.Criticality != string(model.CriticalityNormal) {
		t.Fatalf("default criticality = %q, want normal", q.Criticality)
	}
	hist := must(f.s.JournalForEntity(ctx(), EntityQuestion, q.ID))
	if len(hist) == 0 || !strings.Contains(string(hist[0].Payload), `"criticality":"normal"`) {
		t.Fatalf("asked journal = %+v", hist)
	}

	// Explicit value is stored.
	f.write(func(tx *Tx) error {
		got, err := tx.CreateQuestion(ctx(), a, protocol.QuestionRequest{Text: "ship?", Criticality: "critical"})
		if err != nil {
			return err
		}
		if got.Criticality != "critical" {
			t.Fatalf("criticality = %q, want critical", got.Criticality)
		}
		return nil
	})

	// Unknown value is rejected.
	err := f.s.Write(ctx(), func(tx *Tx) error {
		_, err := tx.CreateQuestion(ctx(), a, protocol.QuestionRequest{Text: "x", Criticality: "urgent"})
		return err
	})
	if err == nil || !strings.Contains(err.Error(), "criticality") {
		t.Fatalf("unknown criticality err = %v", err)
	}
}

// TestAutoAnswerQuestion covers the auto-decision store path: the answer,
// AnsweredBy "auto:opus", and rationale are recorded, the Target resumes, and a
// distinct question.auto_answered journal entry carries the criticality,
// deadline, answer, and rationale.
func TestAutoAnswerQuestion(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	a := f.claim(target, "r1")
	f.run(a.ID, "lease-r1")
	// Raise the question through the completion path so the Target parks in
	// waiting_human, the state the answer resumes from.
	f.write(func(tx *Tx) error {
		_, err := tx.Complete(ctx(), a.ID, protocol.CompleteRequest{LeaseToken: "lease-r1", State: model.WaitingHuman, SessionID: "sess-1", Launches: 1,
			Question: &protocol.QuestionRequest{Text: "which branch?", Options: []string{"main", "dev"}, Criticality: "normal"}, FinishedAt: f.now}, 1)
		return err
	})
	open := must(f.s.OpenQuestions(ctx()))
	if len(open) != 1 {
		t.Fatalf("open questions = %+v", open)
	}

	deadline := f.now.Add(4 * time.Hour)
	f.now = f.now.Add(5 * time.Hour)
	var q *Question
	f.write(func(tx *Tx) error {
		var err error
		q, err = tx.AutoAnswerQuestion(ctx(), open[0].ID, "main", "auto:opus", "main is the integration branch", deadline)
		return err
	})
	if q.Answer != "main" || q.AnsweredBy != "auto:opus" || q.Rationale != "main is the integration branch" || !q.IsAuto() || q.AutoModel() != "opus" {
		t.Fatalf("auto-answered question = %+v", q)
	}
	if tg := must(f.s.GetTarget(ctx(), target.ID)); tg.State != model.Pending {
		t.Fatalf("target after auto-answer = %s, want pending", tg.State)
	}

	// A distinct question.auto_answered entry with the full audit.
	hist := must(f.s.JournalForEntity(ctx(), EntityQuestion, q.ID))
	var found *JournalEntry
	for i := range hist {
		if hist[i].Kind == "question.auto_answered" {
			found = &hist[i]
		}
	}
	if found == nil {
		t.Fatalf("no question.auto_answered entry in %+v", hist)
	}
	var p map[string]any
	if err := json.Unmarshal(found.Payload, &p); err != nil {
		t.Fatal(err)
	}
	if p["by"] != "auto:opus" || p["criticality"] != "normal" || p["answer"] != "main" || p["rationale"] != "main is the integration branch" || p["deadline"] == nil {
		t.Fatalf("auto_answered payload = %v", p)
	}

	// The answer is idempotent: a second answer conflicts.
	err := f.s.Write(ctx(), func(tx *Tx) error {
		_, err := tx.AnswerQuestion(ctx(), open[0].ID, "dev", "human")
		return err
	})
	if !errors.Is(err, ErrConflict) {
		t.Fatalf("double answer: %v", err)
	}
}
