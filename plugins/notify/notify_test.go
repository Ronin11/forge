package main

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// recorder captures notify-send invocations instead of raising them.
type recorder struct {
	calls [][]string
}

func (r *recorder) send(args ...string) error {
	r.calls = append(r.calls, args)
	return nil
}

// The notify-send argv is: -a Forge -u <urgency> [--hint=…] <title> <body>.
// title and body are always the last two; the hint (when present) carries the
// omarchy-exec-argv click route. Helpers keep the assertions index-free.
func titleOf(args []string) string { return args[len(args)-2] }
func bodyOf(args []string) string  { return args[len(args)-1] }
func urgencyOf(args []string) string {
	for i, a := range args {
		if a == "-u" && i+1 < len(args) {
			return args[i+1]
		}
	}
	return ""
}
func hintOf(args []string) string {
	for _, a := range args {
		if strings.HasPrefix(a, "--hint=string:omarchy-exec-argv:") {
			return strings.TrimPrefix(a, "--hint=string:omarchy-exec-argv:")
		}
	}
	return ""
}

// fakeAPI implements forgeAPI in memory; no daemon is ever spun up.
type fakeAPI struct {
	tasks     map[string]*taskDetail
	attention *attentionView
	queue     []queueItem
}

func (f *fakeAPI) Task(_ context.Context, id string) (*taskDetail, error) {
	if d, ok := f.tasks[id]; ok {
		return d, nil
	}
	return nil, errors.New("not found")
}

func (f *fakeAPI) Attention(context.Context) (*attentionView, error) {
	if f.attention == nil {
		return nil, errors.New("attention unavailable")
	}
	return f.attention, nil
}

func (f *fakeAPI) Queue(context.Context) ([]queueItem, error) { return f.queue, nil }

// testClock is an advanceable fixed clock.
type testClock struct{ t time.Time }

func (c *testClock) now() time.Time          { return c.t }
func (c *testClock) advance(d time.Duration) { c.t = c.t.Add(d) }
func newTestClock() *testClock               { return &testClock{t: time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)} }
func testLogger() *slog.Logger               { return slog.New(slog.DiscardHandler) }
func entry(id int64, kind, entityID, payload string) journalEntry {
	return journalEntry{ID: id, Kind: kind, EntityID: entityID, Payload: json.RawMessage(payload)}
}

func newTestNotifier(api *fakeAPI, cfg config) (*notifier, *recorder, *testClock) {
	rec := &recorder{}
	clock := newTestClock()
	n := &notifier{api: api, log: testLogger(), cfg: cfg, send: rec.send, now: clock.now, ui: defaultUI}
	return n, rec, clock
}

const (
	qID = "11111111111111111111111111111111"
	tID = "22222222222222222222222222222222"
	wID = "33333333333333333333333333333333"
	pID = "44444444444444444444444444444444"
)

func TestKindFiltering(t *testing.T) {
	api := &fakeAPI{
		tasks:     map[string]*taskDetail{wID: {Work: workView{ID: wID, RoutineName: "inventory"}}},
		attention: &attentionView{Questions: []questionView{{ID: qID, WorkID: wID, Text: "which branch?"}}},
	}
	n, rec, _ := newTestNotifier(api, defaultConfig())
	ctx := context.Background()

	// Irrelevant kinds notify nothing.
	for _, k := range []string{"work.created", "attempt.completed", "daemon.draining", "kb.note_created", "plugin.enabled"} {
		n.handle(ctx, entry(1, k, "x", `{}`))
	}
	if len(rec.calls) != 0 {
		t.Fatalf("irrelevant kinds produced %d notifications: %v", len(rec.calls), rec.calls)
	}
	// target.transition to a non-failure state notifies nothing.
	n.handle(ctx, entry(2, "target.transition", tID, `{"from":"pending","to":"claimed","work_id":"`+wID+`"}`))
	if len(rec.calls) != 0 {
		t.Fatalf("non-failure transition notified: %v", rec.calls)
	}

	n.handle(ctx, entry(3, "question.asked", qID, `{"attempt_id":"a","target_id":"`+tID+`"}`))
	n.handle(ctx, entry(4, "target.transition", tID, `{"from":"running","to":"failed","reason":"exit_nonzero","work_id":"`+wID+`"}`))
	n.handle(ctx, entry(5, "proposal.created", pID, `{"kind":"routine","target":"inventory"}`))
	if len(rec.calls) != 3 {
		t.Fatalf("got %d notifications, want 3: %v", len(rec.calls), rec.calls)
	}
}

func TestQuestionNotificationContent(t *testing.T) {
	api := &fakeAPI{
		tasks:     map[string]*taskDetail{wID: {Work: workView{ID: wID, RoutineName: "inventory"}}},
		attention: &attentionView{Questions: []questionView{{ID: qID, WorkID: wID, Text: "which branch?"}}},
	}
	n, rec, _ := newTestNotifier(api, defaultConfig())
	n.handle(context.Background(), entry(1, "question.asked", qID, `{}`))
	if len(rec.calls) != 1 {
		t.Fatalf("calls = %v", rec.calls)
	}
	args := rec.calls[0]
	if args[0] != "-a" || args[1] != "Forge" || urgencyOf(args) != "normal" {
		t.Errorf("fixed args wrong: %v", args)
	}
	if titleOf(args) != "Forge: inventory asks" {
		t.Errorf("title = %q", titleOf(args))
	}
	if !strings.Contains(bodyOf(args), "which branch?") || !strings.Contains(bodyOf(args), defaultUI+"/tasks/"+wID) {
		t.Errorf("body = %q", bodyOf(args))
	}
	// Clicking the toast routes to the task via the omarchy-exec-argv hint.
	if h := hintOf(args); !strings.Contains(h, defaultUI+"/tasks/"+wID) || !strings.Contains(h, "xdg-open") {
		t.Errorf("click hint = %q", h)
	}
}

func TestQuestionAttentionFailureDegrades(t *testing.T) {
	n, rec, _ := newTestNotifier(&fakeAPI{}, defaultConfig())
	n.handle(context.Background(), entry(1, "question.asked", qID, `{}`))
	if len(rec.calls) != 1 {
		t.Fatalf("a question with no attention data must still notify: %v", rec.calls)
	}
	if titleOf(rec.calls[0]) != "Forge: question" {
		t.Errorf("title = %q", titleOf(rec.calls[0]))
	}
}

func TestToggles(t *testing.T) {
	api := &fakeAPI{
		attention: &attentionView{Questions: []questionView{{ID: qID, WorkID: wID, Text: "?"}}},
		queue:     []queueItem{{State: "deferred", Reason: "hard_stop:five_hour"}},
	}
	n, rec, _ := newTestNotifier(api, config{})
	ctx := context.Background()
	n.handle(ctx, entry(1, "question.asked", qID, `{}`))
	n.handle(ctx, entry(2, "target.transition", tID, `{"to":"failed","reason":"timeout","work_id":"`+wID+`"}`))
	n.handle(ctx, entry(3, "proposal.created", pID, `{"kind":"routine","target":"x"}`))
	if len(rec.calls) != 0 {
		t.Fatalf("all toggles off but %d notifications: %v", len(rec.calls), rec.calls)
	}
}

func TestFailureDebounce(t *testing.T) {
	api := &fakeAPI{tasks: map[string]*taskDetail{wID: {Work: workView{ID: wID, RoutineName: "touch"}}}}
	n, rec, clock := newTestNotifier(api, config{Failures: true})
	ctx := context.Background()
	fail := func(id int64) {
		n.handle(ctx, entry(id, "target.transition", tID, `{"to":"failed","reason":"exit_nonzero","work_id":"`+wID+`"}`))
	}
	fail(1)
	if len(rec.calls) != 1 {
		t.Fatalf("first failure: %d notifications", len(rec.calls))
	}
	clock.advance(5 * time.Second)
	fail(2)
	clock.advance(5 * time.Second)
	fail(3)
	if len(rec.calls) != 1 {
		t.Fatalf("failures inside the window notified: %v", rec.calls)
	}
	clock.advance(failureDebounce)
	fail(4)
	if len(rec.calls) != 2 {
		t.Fatalf("failure after the window: %d notifications", len(rec.calls))
	}
	if body := bodyOf(rec.calls[1]); !strings.Contains(body, "2 more failures suppressed") {
		t.Errorf("coalesced body = %q", body)
	}
	if title := titleOf(rec.calls[1]); title != "Forge: touch failed" {
		t.Errorf("title = %q", title)
	}
	// The count resets once reported.
	clock.advance(failureDebounce)
	fail(5)
	if body := bodyOf(rec.calls[2]); strings.Contains(body, "suppressed") {
		t.Errorf("count not reset: %q", body)
	}
}

func TestUnverifiedCountsAsFailure(t *testing.T) {
	n, rec, _ := newTestNotifier(&fakeAPI{}, config{Failures: true})
	n.handle(context.Background(), entry(1, "target.transition", tID, `{"to":"unverified","unverified_reason":"checks_failed","work_id":"`+wID+`"}`))
	if len(rec.calls) != 1 {
		t.Fatalf("calls = %v", rec.calls)
	}
	if !strings.Contains(bodyOf(rec.calls[0]), "checks_failed") || !strings.Contains(bodyOf(rec.calls[0]), shortID(tID)) {
		t.Errorf("body = %q", bodyOf(rec.calls[0]))
	}
}

func TestMalformedEntriesSkipped(t *testing.T) {
	n, rec, _ := newTestNotifier(&fakeAPI{}, defaultConfig())
	ctx := context.Background()
	n.handle(ctx, entry(1, "target.transition", tID, `{not json`))
	n.handle(ctx, entry(2, "proposal.created", pID, `"a string, not an object"`))
	n.handle(ctx, entry(3, "proposal.created", pID, `{}`)) // no kind
	if len(rec.calls) != 0 {
		t.Fatalf("malformed payloads notified: %v", rec.calls)
	}
}

func TestThrottleEdge(t *testing.T) {
	api := &fakeAPI{queue: []queueItem{{State: "deferred", Reason: "hard_stop:five_hour"}}}
	n, rec, clock := newTestNotifier(api, config{Throttling: true})
	ctx := context.Background()

	// Any journal activity triggers the first check; the edge notifies once,
	// at critical urgency.
	n.handle(ctx, entry(1, "budget.reset", "budget", `{}`))
	if len(rec.calls) != 1 {
		t.Fatalf("throttle onset: %d notifications", len(rec.calls))
	}
	if args := rec.calls[0]; urgencyOf(args) != "critical" || titleOf(args) != "Forge: budget hard stop" {
		t.Errorf("throttle notification = %v", args)
	}
	// Still throttled: no repeat, even past the check interval.
	clock.advance(throttleCheckEvery + time.Second)
	n.handle(ctx, entry(2, "work.created", wID, `{}`))
	if len(rec.calls) != 1 {
		t.Fatalf("repeat while throttled: %v", rec.calls)
	}
	// Recovery is silent; the next hard stop notifies again.
	api.queue = nil
	clock.advance(throttleCheckEvery + time.Second)
	n.handle(ctx, entry(3, "work.created", wID, `{}`))
	if len(rec.calls) != 1 {
		t.Fatalf("recovery notified: %v", rec.calls)
	}
	api.queue = []queueItem{{State: "deferred", Reason: "hard_stop:seven_day"}}
	clock.advance(throttleCheckEvery + time.Second)
	n.handle(ctx, entry(4, "work.created", wID, `{}`))
	if len(rec.calls) != 2 {
		t.Fatalf("second onset: %d notifications", len(rec.calls))
	}
	// Checks are rate-limited: within the interval the queue is not re-read.
	n.handle(ctx, entry(5, "work.created", wID, `{}`))
	if len(rec.calls) != 2 {
		t.Fatalf("rate-limited check notified: %v", rec.calls)
	}
}

func TestLoadConfig(t *testing.T) {
	dir := t.TempDir()

	// Absent file: everything on.
	cfg, err := loadConfig(filepath.Join(dir, "missing.toml"))
	if err != nil {
		t.Fatal(err)
	}
	if cfg != defaultConfig() {
		t.Errorf("absent file: %+v", cfg)
	}

	// Present file: absent keys keep their defaults.
	path := filepath.Join(dir, "notify.toml")
	if err := os.WriteFile(path, []byte("[notify]\nquestions = false\nthrottling = false\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err = loadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Questions || !cfg.Failures || cfg.Throttling || !cfg.Proposals {
		t.Errorf("parsed config = %+v", cfg)
	}

	// A malformed file is an error, not a silent all-on.
	if err := os.WriteFile(path, []byte("[notify\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := loadConfig(path); err == nil {
		t.Error("malformed config accepted")
	}
}
