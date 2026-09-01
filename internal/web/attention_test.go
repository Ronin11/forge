package web

import (
	"context"
	"strings"
	"testing"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

func TestParseAttentionDecision(t *testing.T) {
	if d := parseAttentionDecision(`{"answer":"main","rationale":"trunk"}`); d.Answer != "main" || d.Rationale != "trunk" {
		t.Errorf("plain: %+v", d)
	}
	// Tolerates surrounding prose / fences.
	if d := parseAttentionDecision("Sure:\n```json\n{\"answer\":\"dev\"}\n```"); d.Answer != "dev" {
		t.Errorf("wrapped: %+v", d)
	}
	// Unparseable → empty answer, so the sweep leaves the question open.
	if d := parseAttentionDecision("no json here"); d.Answer != "" {
		t.Errorf("fallback: %+v", d)
	}
}

func TestAttentionDeadline(t *testing.T) {
	cfg := config.AttentionConfig{WaitActiveMinutes: 240, WaitQuietMinutes: 20, Model: "opus"}
	quiet := config.QuietHoursConfig{Start: "22:00", End: "06:00"}
	asked := time.Date(2026, 8, 30, 22, 0, 0, 0, time.UTC)
	q := func(crit string) store.Question {
		return store.Question{Criticality: crit, AskedAt: asked}
	}
	active := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC) // outside quiet hours
	night := time.Date(2026, 8, 30, 23, 0, 0, 0, time.UTC)  // inside quiet hours

	// Active hours use the long wait; quiet hours use the short one.
	if d, ok := attentionDeadline(q("normal"), active, cfg, quiet); !ok || !d.Equal(asked.Add(240*time.Minute)) {
		t.Errorf("normal active = %v %v", d, ok)
	}
	if d, ok := attentionDeadline(q("normal"), night, cfg, quiet); !ok || !d.Equal(asked.Add(20*time.Minute)) {
		t.Errorf("normal quiet = %v %v", d, ok)
	}
	// low burns down at the quiet wait even during active hours.
	if d, ok := attentionDeadline(q("low"), active, cfg, quiet); !ok || !d.Equal(asked.Add(20*time.Minute)) {
		t.Errorf("low active = %v %v", d, ok)
	}
	// Empty criticality defaults to normal (auto-decidable).
	if _, ok := attentionDeadline(q(""), active, cfg, quiet); !ok {
		t.Error("empty criticality should auto-decide")
	}
	// critical never auto-decides.
	if _, ok := attentionDeadline(q("critical"), active, cfg, quiet); ok {
		t.Error("critical should not auto-decide")
	}
	// auto_decide off disables the whole mechanism.
	off := false
	if _, ok := attentionDeadline(q("normal"), active, config.AttentionConfig{AutoDecide: &off, WaitActiveMinutes: 240, WaitQuietMinutes: 20}, quiet); ok {
		t.Error("auto_decide off should not auto-decide")
	}
}

// TestSweepAutoDecides drives the sweep end to end: a past-deadline normal
// question is auto-answered by the injected decider (and its Work resumes),
// while a critical one and a not-yet-due one are left for a human.
func TestSweepAutoDecides(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.srv.attentionCfg = config.AttentionConfig{WaitActiveMinutes: 240, WaitQuietMinutes: 20, Model: "opus"}

	var calls int
	var sawText string
	h.srv.modelCall = func(_ context.Context, _, user, m string) (string, error) {
		calls++
		sawText = user
		if m != "opus" {
			t.Errorf("decider model = %q, want opus", m)
		}
		return `{"answer":"main","rationale":"main is the trunk"}`, nil
	}

	// raise completes a fresh attempt into waiting_human with one question.
	raise := func(reqID, text, criticality string) string {
		h.run("inventory")
		c := h.mustClaim(reqID)
		h.heartbeat(c, model.Preparing, 0)
		h.heartbeat(c, model.Running, 1)
		req := completeRequest(model.WaitingHuman, h.clock.Now())
		req.Question = &protocol.QuestionRequest{Text: text, Criticality: criticality}
		h.complete(c, req)
		return c.TargetID
	}

	// Two questions asked now; advance past the 240m active wait; a third asked
	// late so it is not yet due at sweep time.
	normalTarget := raise("r1", "q-normal", "normal")
	raise("r2", "q-critical", "critical")
	h.clock.Advance(5 * time.Hour)
	raise("r3", "q-late", "normal")

	h.srv.sweepAttention(context.Background())

	if calls != 1 {
		t.Fatalf("decider calls = %d, want 1", calls)
	}
	if want := "q-normal"; !strings.Contains(sawText, want) {
		t.Errorf("decider prompt %q lacks %q", sawText, want)
	}

	open, err := h.st.OpenQuestions(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	// Only the critical and the not-yet-due question remain open.
	texts := map[string]bool{}
	for _, q := range open {
		texts[q.Text] = true
	}
	if texts["q-normal"] {
		t.Error("q-normal should have been auto-answered")
	}
	if !texts["q-critical"] || !texts["q-late"] {
		t.Errorf("open questions = %v, want q-critical and q-late", texts)
	}
	// The auto-answered question's Work resumed to pending.
	if tg := h.target(normalTarget); tg.State != model.Pending {
		t.Errorf("auto-answered target = %s, want pending", tg.State)
	}

	// A second sweep is a no-op — the answered question is no longer open.
	h.srv.sweepAttention(context.Background())
	if calls != 1 {
		t.Errorf("second sweep re-decided: calls = %d", calls)
	}
}
