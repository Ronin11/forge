package main

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// fakePoster records the cards a bridge would have posted.
type fakePoster struct {
	notes []note
	err   error
}

func (f *fakePoster) post(_ context.Context, n note) error {
	if f.err != nil {
		return f.err
	}
	f.notes = append(f.notes, n)
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
func (f *fakeAPI) Assistant(_ context.Context, _, text string) (string, error) {
	f.lastSent = text
	return f.assistant, f.assistErr
}

// fakeReader serves scripted channel messages.
type fakeReader struct {
	msgs  []incoming
	allow map[string]bool
}

func (f *fakeReader) messages(_ context.Context, after time.Time) ([]incoming, error) {
	var out []incoming
	for _, m := range f.msgs {
		if m.Created.After(after) {
			out = append(out, m)
		}
	}
	return out, nil
}

func (f *fakeReader) allowed(from string) bool {
	if f.allow == nil {
		return true
	}
	return f.allow[from]
}

func newTestBridge(t *testing.T, api *fakeAPI, out *fakePoster, in *fakeReader) *bridge {
	t.Helper()
	cfg := defaultConfig()
	return &bridge{
		api: api, out: out, in: in, cfg: cfg, log: slog.New(slog.DiscardHandler),
		now: time.Now, seen: map[string]bool{}, stateFile: filepath.Join(t.TempDir(), "state.json"),
	}
}

func TestQuestionCardIsSentOnceWithAnswerHint(t *testing.T) {
	api := &fakeAPI{attention: attentionView{Questions: []questionView{{ID: "q1", WorkID: "3f69f96a1122", Text: "which branch?"}}}}
	out := &fakePoster{}
	b := newTestBridge(t, api, out, nil)
	b.handle(context.Background(), journalEntry{ID: 1, Kind: "question.asked"})
	b.handle(context.Background(), journalEntry{ID: 2, Kind: "question.asked"}) // replay
	if len(out.notes) != 1 {
		t.Fatalf("posted %d cards, want 1", len(out.notes))
	}
	n := out.notes[0]
	if !strings.Contains(n.Text, "which branch?") || !strings.Contains(n.Text, "/answer 3f69f96a") {
		t.Fatalf("card text lacks the question or the answer hint: %q", n.Text)
	}
	if n.URL != defaultUI+"/tasks/3f69f96a1122" {
		t.Errorf("url = %q", n.URL)
	}
}

func TestFailureCardDebouncesAndReportsSuppressed(t *testing.T) {
	api := &fakeAPI{}
	out := &fakePoster{}
	b := newTestBridge(t, api, out, nil)
	now := time.Now()
	b.now = func() time.Time { return now }
	failure := func() journalEntry {
		return journalEntry{ID: 1, Kind: "target.transition", EntityID: "t1",
			Payload: json.RawMessage(`{"to":"failed","reason":"nonzero_exit","work_id":"w1","repository":"equitizr"}`)}
	}
	b.handle(context.Background(), failure())
	b.handle(context.Background(), failure()) // inside the window: suppressed
	if len(out.notes) != 1 {
		t.Fatalf("posted %d cards, want 1", len(out.notes))
	}
	now = now.Add(failureDebounce + time.Second)
	b.handle(context.Background(), failure())
	if len(out.notes) != 2 {
		t.Fatalf("posted %d cards, want 2", len(out.notes))
	}
	if !strings.Contains(out.notes[1].Text, "1 more failures suppressed") {
		t.Errorf("second card should report the suppressed one: %q", out.notes[1].Text)
	}
	if !out.notes[0].Urgent {
		t.Error("a failure card should be urgent")
	}
}

// A transition to a good state is not news.
func TestGoodTransitionIsSilent(t *testing.T) {
	out := &fakePoster{}
	b := newTestBridge(t, &fakeAPI{}, out, nil)
	b.handle(context.Background(), journalEntry{ID: 1, Kind: "target.transition", Payload: json.RawMessage(`{"to":"succeeded"}`)})
	if len(out.notes) != 0 {
		t.Fatalf("posted %d cards, want 0", len(out.notes))
	}
}

func TestThrottleCardIsEdgeTriggered(t *testing.T) {
	api := &fakeAPI{queue: []queueItem{{State: "deferred", Reason: "hard_stop:five_hour"}}}
	out := &fakePoster{}
	b := newTestBridge(t, api, out, nil)
	now := time.Now()
	b.now = func() time.Time { return now }
	b.checkThrottle(context.Background())
	now = now.Add(throttleCheckEvery + time.Second)
	b.checkThrottle(context.Background()) // still throttled: no second card
	if len(out.notes) != 1 {
		t.Fatalf("posted %d cards, want 1", len(out.notes))
	}
	if !strings.Contains(out.notes[0].Title, "hard stop") {
		t.Errorf("title = %q", out.notes[0].Title)
	}
	// Clearing and re-tripping cards again.
	api.queue = nil
	now = now.Add(throttleCheckEvery + time.Second)
	b.checkThrottle(context.Background())
	api.queue = []queueItem{{State: "deferred", Reason: "hard_stop:seven_day"}}
	now = now.Add(throttleCheckEvery + time.Second)
	b.checkThrottle(context.Background())
	if len(out.notes) != 2 {
		t.Fatalf("posted %d cards, want 2 after the edge re-trips", len(out.notes))
	}
}

func TestInboundAnswerResolvesQuestion(t *testing.T) {
	api := &fakeAPI{attention: attentionView{Questions: []questionView{{ID: "q1", WorkID: "3f69f96a1122", Text: "which branch?"}}}}
	out := &fakePoster{}
	b := newTestBridge(t, api, out, &fakeReader{})
	b.seen["q1"] = true
	b.command(context.Background(), "/answer 3f69f96a use main", "nate@example.com")
	if api.answers["q1"] != "use main" {
		t.Fatalf("answer recorded = %q", api.answers["q1"])
	}
	if b.seen["q1"] {
		t.Error("an answered question must be re-cardable if it is asked again")
	}
	if len(out.notes) != 1 || !strings.Contains(out.notes[0].Text, "Answered task 3f69f96a") {
		t.Fatalf("confirmation card = %+v", out.notes)
	}
}

func TestInboundBareTextGoesToTheConcierge(t *testing.T) {
	api := &fakeAPI{assistant: "Filed it as task ab12cd34."}
	out := &fakePoster{}
	b := newTestBridge(t, api, out, &fakeReader{})
	b.command(context.Background(), "fix the login bug", "nate@example.com")
	if api.lastSent != "fix the login bug" {
		t.Fatalf("concierge got %q", api.lastSent)
	}
	if len(out.notes) != 1 || out.notes[0].Text != "Filed it as task ab12cd34." {
		t.Fatalf("reply card = %+v", out.notes)
	}
}

func TestInboundConciergeErrorIsReported(t *testing.T) {
	api := &fakeAPI{assistErr: &apiError{Status: 503, Body: "assistant unavailable"}}
	out := &fakePoster{}
	b := newTestBridge(t, api, out, &fakeReader{})
	b.command(context.Background(), "status?", "nate@example.com")
	if len(out.notes) != 1 || !strings.Contains(out.notes[0].Text, "assistant unavailable") {
		t.Fatalf("error card = %+v", out.notes)
	}
}

func TestConsumeDedupesFiltersAndAdvancesCursor(t *testing.T) {
	base := time.Now().Add(-time.Hour)
	api := &fakeAPI{assistant: "ok"}
	out := &fakePoster{}
	in := &fakeReader{allow: map[string]bool{"nate@example.com": true}}
	b := newTestBridge(t, api, out, in)
	m := incoming{ID: "m1", From: "nate@example.com", Text: "do a thing", Created: base}
	b.consume(context.Background(), m)
	b.consume(context.Background(), m) // same id again: ignored
	b.consume(context.Background(), incoming{ID: "m2", From: "stranger@example.com", Text: "rm -rf", Created: base.Add(time.Second)})
	if len(out.notes) != 1 {
		t.Fatalf("posted %d replies, want 1 (dupes and strangers ignored)", len(out.notes))
	}
	if api.lastSent != "do a thing" {
		t.Errorf("concierge saw %q", api.lastSent)
	}
	if !b.cursor().Equal(base.Add(time.Second)) {
		t.Errorf("cursor = %v, want the newest message time", b.cursor())
	}
	// State round-trips, so a restart does not replay the same commands.
	b2 := newTestBridge(t, api, out, in)
	b2.stateFile = b.stateFile
	b2.loadState()
	if !b2.alreadyActed("m1") || !b2.cursor().Equal(b.cursor()) {
		t.Errorf("state did not survive a restart: acted=%v cursor=%v", b2.alreadyActed("m1"), b2.cursor())
	}
}

func TestCommandPrefixGatesTheChannel(t *testing.T) {
	if _, ok := stripPrefix("hello", ""); !ok {
		t.Error("no prefix configured means every message counts")
	}
	if _, ok := stripPrefix("hello", "!forge"); ok {
		t.Error("a message without the prefix must be ignored")
	}
	got, ok := stripPrefix("!Forge  fix the bug", "!forge")
	if !ok || got != "fix the bug" {
		t.Errorf("stripPrefix = %q, %v", got, ok)
	}
	if _, ok := stripPrefix("!forge   ", "!forge"); ok {
		t.Error("a prefix with no command is not a command")
	}
}

func TestAdaptivePayloadShape(t *testing.T) {
	body := adaptivePayload(note{Title: "T", Text: "B", URL: "http://u", Facts: [][2]string{{"Task", "abc"}}, Urgent: true})
	raw, err := json.Marshal(body)
	if err != nil {
		t.Fatal(err)
	}
	var got struct {
		Type        string `json:"type"`
		Attachments []struct {
			ContentType string `json:"contentType"`
			Content     struct {
				Type    string `json:"type"`
				Version string `json:"version"`
				Body    []struct {
					Type  string `json:"type"`
					Text  string `json:"text"`
					Color string `json:"color"`
				} `json:"body"`
				Actions []struct {
					Type string `json:"type"`
					URL  string `json:"url"`
				} `json:"actions"`
			} `json:"content"`
		} `json:"attachments"`
	}
	if err := json.Unmarshal(raw, &got); err != nil {
		t.Fatal(err)
	}
	if got.Type != "message" || len(got.Attachments) != 1 {
		t.Fatalf("envelope wrong: %s", raw)
	}
	a := got.Attachments[0]
	if a.ContentType != "application/vnd.microsoft.card.adaptive" || a.Content.Type != "AdaptiveCard" {
		t.Fatalf("attachment wrong: %s", raw)
	}
	if a.Content.Body[0].Text != "T" || a.Content.Body[0].Color != "Attention" {
		t.Errorf("title block wrong: %s", raw)
	}
	if len(a.Content.Actions) != 1 || a.Content.Actions[0].URL != "http://u" {
		t.Errorf("action wrong: %s", raw)
	}
}

func TestMessageCardPayloadShape(t *testing.T) {
	raw, err := json.Marshal(messageCardPayload(note{Title: "T", Text: "B", URL: "http://u"}))
	if err != nil {
		t.Fatal(err)
	}
	var got map[string]any
	if err := json.Unmarshal(raw, &got); err != nil {
		t.Fatal(err)
	}
	if got["@type"] != "MessageCard" || got["title"] != "T" {
		t.Fatalf("card wrong: %s", raw)
	}
	if _, ok := got["potentialAction"]; !ok {
		t.Errorf("missing the OpenUri action: %s", raw)
	}
}

// The webhook posts JSON and surfaces a non-2xx as an error a caller can log.
func TestWebhookPostAndError(t *testing.T) {
	var seen []byte
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, rerr := io.ReadAll(io.LimitReader(r.Body, 1<<16))
		if rerr != nil {
			t.Errorf("read webhook body: %v", rerr)
		}
		seen = body
		if r.URL.Path == "/bad" {
			http.Error(w, "webhook expired", http.StatusGone)
			return
		}
		w.WriteHeader(http.StatusOK)
	}))
	defer srv.Close()
	if err := newWebhook(srv.URL, formatAdaptive).post(context.Background(), note{Title: "hi"}); err != nil {
		t.Fatalf("post: %v", err)
	}
	if !strings.Contains(string(seen), "AdaptiveCard") {
		t.Errorf("posted body = %s", seen)
	}
	err := newWebhook(srv.URL+"/bad", formatAdaptive).post(context.Background(), note{Title: "hi"})
	if err == nil || !strings.Contains(err.Error(), "410") {
		t.Fatalf("want a 410 error, got %v", err)
	}
}

func TestErrMessageUnwrapsAPIError(t *testing.T) {
	if got := errMessage(&apiError{Status: 400, Body: " bad request "}); got != "bad request" {
		t.Errorf("errMessage = %q", got)
	}
	if got := errMessage(errors.New("boom")); got != "boom" {
		t.Errorf("errMessage = %q", got)
	}
}
