package web

import (
	"context"
	"fmt"
	"net/http"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// The ask budget (routines.max_questions, default 3): three questions pass,
// the fourth fails the Target with ask_budget_exhausted, journaled, with
// facts recorded and no fourth question row.
func TestAskBudgetExhausted(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	var out workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "needs many answers", Repositories: []string{"equitizr"}}, &out, http.StatusCreated)
	targetID := out.Targets[0].ID

	ask := func(n int) protocol.CompleteResponse {
		c := h.mustClaim(fmt.Sprintf("ask-r%d", n))
		h.heartbeat(c, model.Preparing, 0)
		h.heartbeat(c, model.Running, n)
		req := completeRequest(model.WaitingHuman, h.clock.Now())
		req.Question = &protocol.QuestionRequest{Text: fmt.Sprintf("q%d?", n)}
		return h.complete(c, req)
	}
	for i := 1; i <= 3; i++ {
		if done := ask(i); done.State != model.WaitingHuman {
			t.Fatalf("question %d refused: %+v", i, done)
		}
		var att attention
		h.call(http.MethodGet, "/api/v1/attention", nil, &att, http.StatusOK)
		if len(att.Questions) != 1 {
			t.Fatalf("open questions after ask %d = %+v", i, att.Questions)
		}
		h.call(http.MethodPost, "/api/v1/questions/"+att.Questions[0].ID+"/answer", answerRequest{Answer: "a"}, nil, http.StatusOK)
	}

	done := ask(4)
	if done.State != model.Failed {
		t.Fatalf("fourth question = %+v, want failed", done)
	}
	tg := h.target(targetID)
	if tg.State != model.Failed || tg.FailureReason != model.ReasonAskBudgetExhausted {
		t.Errorf("target = %s/%s, want failed/ask_budget_exhausted", tg.State, tg.FailureReason)
	}
	qs, err := h.st.QuestionsForWork(context.Background(), out.Work.ID)
	if err != nil || len(qs) != 3 {
		t.Errorf("questions = %d (%v), want 3 (no fourth row)", len(qs), err)
	}
	entries, err := h.st.JournalForEntity(context.Background(), "target", targetID)
	if err != nil || !hasKind(entries, "question.budget_exhausted") {
		t.Errorf("journal lacks question.budget_exhausted (%v)", err)
	}
	att := h.attempt(qs[0].AttemptID)
	facts, err := h.st.FactsForAttempt(context.Background(), att.ID)
	if err != nil || facts.QuestionsAsked == nil || *facts.QuestionsAsked != 3 {
		t.Errorf("facts questions_asked = %+v (%v)", facts, err)
	}
}

// A retry is a fresh attempt with a fresh ask budget: exhausting the budget
// fails the Target, but the retried Target's new attempt may ask again — an
// inherited spent budget doomed any approval-gated task's second life
// (crashbyforge, 2026-09-07).
func TestAskBudgetResetsOnRetry(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	var out workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "needs many answers", Repositories: []string{"equitizr"}}, &out, http.StatusCreated)
	targetID := out.Targets[0].ID

	ask := func(n int) protocol.CompleteResponse {
		c := h.mustClaim(fmt.Sprintf("ask-rr%d", n))
		h.heartbeat(c, model.Preparing, 0)
		h.heartbeat(c, model.Running, n)
		req := completeRequest(model.WaitingHuman, h.clock.Now())
		req.Question = &protocol.QuestionRequest{Text: fmt.Sprintf("q%d?", n)}
		return h.complete(c, req)
	}
	answer := func() {
		var att attention
		h.call(http.MethodGet, "/api/v1/attention", nil, &att, http.StatusOK)
		h.call(http.MethodPost, "/api/v1/questions/"+att.Questions[0].ID+"/answer", answerRequest{Answer: "a"}, nil, http.StatusOK)
	}
	for i := 1; i <= 3; i++ {
		if done := ask(i); done.State != model.WaitingHuman {
			t.Fatalf("question %d refused: %+v", i, done)
		}
		answer()
	}
	if done := ask(4); done.State != model.Failed {
		t.Fatalf("fourth question = %+v, want failed", done)
	}

	h.call(http.MethodPost, "/api/v1/targets/"+targetID+"/retry", nil, nil, http.StatusOK)
	if done := ask(5); done.State != model.WaitingHuman {
		t.Fatalf("post-retry question refused: %+v — the retry must reset the ask budget", done)
	}
	if tg := h.target(targetID); tg.State != model.WaitingHuman {
		t.Errorf("target = %s, want waiting_human", tg.State)
	}
}

// The completion's needs_input restates the question forge_ask already
// created mid-turn; the twin sent the operator a duplicate approval message
// on every checkpoint (2026-09-07). Same attempt + same text dedupes: an
// open prior is the gate (one row, one message), and an answered prior
// skips the wait entirely — the human beat the checkpoint.
func TestCheckpointQuestionDeduplicated(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	var out workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "approval flow", Repositories: []string{"equitizr"}}, &out, http.StatusCreated)

	c := h.mustClaim("dedupe-r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 2)
	// forge_ask mid-turn.
	err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		a, err := tx.GetAttempt(context.Background(), c.AttemptID)
		if err != nil {
			return err
		}
		_, err = tx.CreateQuestion(context.Background(), a, protocol.QuestionRequest{Text: "Approve the thing?"})
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	// The checkpoint exit restates it.
	req := completeRequest(model.WaitingHuman, h.clock.Now())
	req.Question = &protocol.QuestionRequest{Text: "Approve the thing?"}
	if done := h.complete(c, req); done.State != model.WaitingHuman {
		t.Fatalf("complete = %+v", done)
	}
	qs, err := h.st.QuestionsForWork(context.Background(), out.Work.ID)
	if err != nil || len(qs) != 1 {
		t.Fatalf("questions = %d (%v), want 1 — no twin", len(qs), err)
	}

	// Answer, resume, ask again mid-turn, human answers BEFORE the exit:
	// the checkpoint completion must skip the wait and requeue.
	h.call(http.MethodPost, "/api/v1/questions/"+qs[0].ID+"/answer", answerRequest{Answer: "yes"}, nil, http.StatusOK)
	c2 := h.mustClaim("dedupe-r2")
	h.heartbeat(c2, model.Preparing, 0)
	h.heartbeat(c2, model.Running, 4)
	err = h.st.Write(context.Background(), func(tx *store.Tx) error {
		a, err := tx.GetAttempt(context.Background(), c2.AttemptID)
		if err != nil {
			return err
		}
		q, err := tx.CreateQuestion(context.Background(), a, protocol.QuestionRequest{Text: "Approve round two?"})
		if err != nil {
			return err
		}
		_, err = tx.AnswerQuestion(context.Background(), q.ID, "yes", "human")
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	req2 := completeRequest(model.WaitingHuman, h.clock.Now())
	req2.Question = &protocol.QuestionRequest{Text: "Approve round two?"}
	h.complete(c2, req2)
	if tg := h.target(out.Targets[0].ID); tg.State != model.Pending {
		t.Fatalf("target = %s, want pending (answered prior skips the wait)", tg.State)
	}
}
