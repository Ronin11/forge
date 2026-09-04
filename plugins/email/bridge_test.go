package main

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// fakeMailer records what would have been sent.
type fakeMailer struct {
	sent []outgoing
	err  error
}

func (f *fakeMailer) send(_ context.Context, m outgoing) error {
	if f.err != nil {
		return f.err
	}
	f.sent = append(f.sent, m)
	return nil
}

// fakeAPI is a scripted daemon.
type fakeAPI struct {
	attention attentionView
	queue     []queueItem
	answers   map[string]string
	assistant string
	assistErr error
	lastSent  string
	lastFrom  string
}

func (f *fakeAPI) Attention(context.Context) (*attentionView, error) { return &f.attention, nil }
func (f *fakeAPI) Task(context.Context, string) (*taskDetail, error) {
	return &taskDetail{Work: workView{RoutineName: "inventory"}}, nil
}
func (f *fakeAPI) Queue(context.Context) ([]queueItem, error) { return f.queue, nil }
func (f *fakeAPI) AnswerQuestion(_ context.Context, id, answer string) error {
	if f.answers == nil {
		f.answers = map[string]string{}
	}
	f.answers[id] = answer
	return nil
}
func (f *fakeAPI) Assistant(_ context.Context, sender, text string) (string, error) {
	f.lastFrom, f.lastSent = sender, text
	return f.assistant, f.assistErr
}

func newTestBridge(t *testing.T, api *fakeAPI, out *fakeMailer) *bridge {
	t.Helper()
	cfg := defaultConfig()
	cfg.From, cfg.To = "forge@example.com", "nate@example.com"
	return &bridge{
		api: api, out: out, cfg: cfg, log: slog.New(slog.DiscardHandler),
		now: time.Now, seen: map[string]bool{}, stateFile: filepath.Join(t.TempDir(), "state.json"),
	}
}

func TestQuestionMailCarriesTheMarkerOnce(t *testing.T) {
	api := &fakeAPI{attention: attentionView{Questions: []questionView{{ID: "q1", WorkID: "3f69f96a1122", Text: "which branch?"}}}}
	out := &fakeMailer{}
	b := newTestBridge(t, api, out)
	b.handle(context.Background(), journalEntry{ID: 1, Kind: "question.asked"})
	b.handle(context.Background(), journalEntry{ID: 2, Kind: "question.asked"}) // replay
	if len(out.sent) != 1 {
		t.Fatalf("sent %d mails, want 1", len(out.sent))
	}
	m := out.sent[0]
	if taskRefFromSubject(m.Subject) != "3f69f96a" {
		t.Fatalf("subject lacks the task marker: %q", m.Subject)
	}
	if !strings.Contains(m.Subject, "inventory") {
		t.Errorf("subject should name the routine: %q", m.Subject)
	}
	if !strings.Contains(m.Body, "which branch?") || !strings.Contains(m.Body, "/tasks/3f69f96a1122") {
		t.Errorf("body = %q", m.Body)
	}
}

func TestFailureMailDebouncesAndReportsSuppressed(t *testing.T) {
	out := &fakeMailer{}
	b := newTestBridge(t, &fakeAPI{}, out)
	now := time.Now()
	b.now = func() time.Time { return now }
	failure := journalEntry{ID: 1, Kind: "target.transition", EntityID: "t1",
		Payload: json.RawMessage(`{"to":"unverified","unverified_reason":"l0:changes_mismatch","work_id":"w1234567890","repository":"equitizr"}`)}
	b.handle(context.Background(), failure)
	b.handle(context.Background(), failure)
	if len(out.sent) != 1 {
		t.Fatalf("sent %d mails, want 1", len(out.sent))
	}
	if !strings.Contains(out.sent[0].Body, "l0:changes_mismatch") || !strings.Contains(out.sent[0].Body, "equitizr") {
		t.Errorf("body = %q", out.sent[0].Body)
	}
	now = now.Add(failureDebounce + time.Second)
	b.handle(context.Background(), failure)
	if len(out.sent) != 2 || !strings.Contains(out.sent[1].Body, "1 more failures") {
		t.Fatalf("second mail should report the suppressed one: %+v", out.sent)
	}
}

func TestGoodTransitionIsSilent(t *testing.T) {
	out := &fakeMailer{}
	b := newTestBridge(t, &fakeAPI{}, out)
	b.handle(context.Background(), journalEntry{ID: 1, Kind: "target.transition", Payload: json.RawMessage(`{"to":"succeeded"}`)})
	if len(out.sent) != 0 {
		t.Fatalf("sent %d mails, want 0", len(out.sent))
	}
}

func TestThrottleMailIsEdgeTriggered(t *testing.T) {
	api := &fakeAPI{queue: []queueItem{{State: "deferred", Reason: "hard_stop:five_hour"}}}
	out := &fakeMailer{}
	b := newTestBridge(t, api, out)
	now := time.Now()
	b.now = func() time.Time { return now }
	b.checkThrottle(context.Background())
	now = now.Add(throttleCheckEvery + time.Second)
	b.checkThrottle(context.Background())
	if len(out.sent) != 1 || !strings.Contains(out.sent[0].Subject, "five_hour") {
		t.Fatalf("mails = %+v", out.sent)
	}
}

// A reply carrying the marker answers that task's question — the mechanism the
// whole inbound design rests on.
func TestReplyAnswersTheMarkedQuestion(t *testing.T) {
	api := &fakeAPI{attention: attentionView{Questions: []questionView{{ID: "q1", WorkID: "3f69f96a1122", Text: "which branch?"}}}}
	out := &fakeMailer{}
	b := newTestBridge(t, api, out)
	b.seen["q1"] = true
	b.consume(context.Background(), inboundMail{
		ID: "1", From: "Nate <nate@example.com>", Subject: "Re: [forge #3f69f96a] Forge: inventory needs input",
		MessageID: "<r1@example.com>",
		Body:      "use main\n\nOn Tue, Sep 1, 2026 Forge <forge@example.com> wrote:\n> which branch?",
	})
	if api.answers["q1"] != "use main" {
		t.Fatalf("answer recorded = %q (quoted text must be stripped)", api.answers["q1"])
	}
	if b.seen["q1"] {
		t.Error("an answered question must be re-mailable if it is asked again")
	}
	if len(out.sent) != 1 || !strings.Contains(out.sent[0].Body, "Answered task 3f69f96a") {
		t.Fatalf("confirmation = %+v", out.sent)
	}
	if out.sent[0].InReplyTo != "<r1@example.com>" {
		t.Errorf("the confirmation should thread: %+v", out.sent[0])
	}
}

func TestReplyToAnAlreadyClosedQuestionSaysSo(t *testing.T) {
	out := &fakeMailer{}
	b := newTestBridge(t, &fakeAPI{}, out)
	b.consume(context.Background(), inboundMail{ID: "1", From: "nate@example.com", Subject: "Re: [forge #abcd1234] q", Body: "yes"})
	if len(out.sent) != 1 || !strings.Contains(out.sent[0].Body, "No open question") {
		t.Fatalf("mails = %+v", out.sent)
	}
}

func TestNewMailGoesToTheConcierge(t *testing.T) {
	api := &fakeAPI{assistant: "Filed it as task ab12cd34."}
	out := &fakeMailer{}
	b := newTestBridge(t, api, out)
	b.consume(context.Background(), inboundMail{ID: "1", From: "nate@example.com", Subject: "fix the login bug", Body: "the login page 500s on submit"})
	if api.lastSent != "the login page 500s on submit" {
		t.Fatalf("concierge got %q", api.lastSent)
	}
	if api.lastFrom != "email:nate@example.com" {
		t.Errorf("session key = %q", api.lastFrom)
	}
	if len(out.sent) != 1 || !strings.Contains(out.sent[0].Body, "Filed it as task ab12cd34.") {
		t.Fatalf("reply = %+v", out.sent)
	}
	if out.sent[0].Subject != "Re: fix the login bug" {
		t.Errorf("subject = %q", out.sent[0].Subject)
	}
}

// The three ways inbound mail is refused: a stranger, a machine, and Forge's
// own notification landing back in the inbox.
func TestInboundRefusals(t *testing.T) {
	api := &fakeAPI{assistant: "ok"}
	out := &fakeMailer{}
	b := newTestBridge(t, api, out)
	b.consume(context.Background(), inboundMail{ID: "1", From: "stranger@example.com", Subject: "hi", Body: "rm -rf"})
	b.consume(context.Background(), inboundMail{ID: "2", From: "nate@example.com", Subject: "Out of office", Body: "I am away", Auto: true})
	b.consume(context.Background(), inboundMail{ID: "3", From: "forge@example.com", Subject: "loop?", Body: "echo"})
	b.consume(context.Background(), inboundMail{ID: "4", From: "nate@example.com", Subject: "Re: q", Body: "> only quoted\n"})
	if len(out.sent) != 0 || api.lastSent != "" {
		t.Fatalf("nothing should have been acted on: sent=%+v concierge=%q", out.sent, api.lastSent)
	}
}

func TestAllowedSendersReplaceTheToAddress(t *testing.T) {
	api := &fakeAPI{assistant: "ok"}
	out := &fakeMailer{}
	b := newTestBridge(t, api, out)
	b.cfg.AllowedSenders = []string{"sam@example.com"}
	b.consume(context.Background(), inboundMail{ID: "1", From: "nate@example.com", Subject: "hi", Body: "do a thing"})
	if len(out.sent) != 0 {
		t.Fatalf("the to address is not automatically allowed once allowed_senders is set: %+v", out.sent)
	}
	b.consume(context.Background(), inboundMail{ID: "2", From: "sam@example.com", Subject: "hi", Body: "do a thing"})
	if len(out.sent) != 1 {
		t.Fatalf("a listed sender should be honored: %+v", out.sent)
	}
}

func TestConsumeDedupesAcrossRestarts(t *testing.T) {
	api := &fakeAPI{assistant: "ok"}
	out := &fakeMailer{}
	b := newTestBridge(t, api, out)
	m := inboundMail{ID: "42", From: "nate@example.com", Subject: "hi", Body: "do a thing"}
	b.consume(context.Background(), m)
	b.consume(context.Background(), m)
	if len(out.sent) != 1 {
		t.Fatalf("sent %d replies, want 1", len(out.sent))
	}
	b2 := newTestBridge(t, api, out)
	b2.stateFile = b.stateFile
	b2.loadState()
	if !b2.alreadyActed("42") {
		t.Error("state did not survive a restart")
	}
}

func TestConciergeErrorIsMailedBack(t *testing.T) {
	api := &fakeAPI{assistErr: &apiError{Status: 503, Body: "assistant unavailable"}}
	out := &fakeMailer{}
	b := newTestBridge(t, api, out)
	b.consume(context.Background(), inboundMail{ID: "1", From: "nate@example.com", Subject: "status?", Body: "how is it going"})
	if len(out.sent) != 1 || !strings.Contains(out.sent[0].Body, "assistant unavailable") {
		t.Fatalf("mails = %+v", out.sent)
	}
}

// A send failure is logged, never fatal: the loop must survive a dead mail
// server.
func TestSendFailureIsNotFatal(t *testing.T) {
	out := &fakeMailer{err: errors.New("connection refused")}
	b := newTestBridge(t, &fakeAPI{}, out)
	b.mail(context.Background(), outgoing{Subject: "s", Body: "b"})
}

func TestErrMessageUnwrapsAPIError(t *testing.T) {
	if got := errMessage(&apiError{Status: 400, Body: " bad request "}); got != "bad request" {
		t.Errorf("errMessage = %q", got)
	}
	if got := errMessage(errors.New("boom")); got != "boom" {
		t.Errorf("errMessage = %q", got)
	}
}
