package controlplane

import (
	"context"
	"fmt"
	"net/http"
	"testing"

	"forge/internal/model"
	"forge/internal/protocol"
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
