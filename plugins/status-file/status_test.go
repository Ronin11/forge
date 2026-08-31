package main

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"
)

// fakeAPI implements forgeAPI in memory; no daemon is ever spun up.
type fakeAPI struct {
	queue        []queueItem
	queueErr     error
	tasks        map[string]*taskDetail
	attention    *attentionView
	attentionErr error
	usage        *usageView
	usageErr     error
}

func (f *fakeAPI) Queue(context.Context) ([]queueItem, error) { return f.queue, f.queueErr }

func (f *fakeAPI) Task(_ context.Context, id string) (*taskDetail, error) {
	if d, ok := f.tasks[id]; ok {
		return d, nil
	}
	return nil, errors.New("not found")
}

func (f *fakeAPI) Attention(context.Context) (*attentionView, error) {
	if f.attentionErr != nil {
		return nil, f.attentionErr
	}
	if f.attention == nil {
		return &attentionView{}, nil
	}
	return f.attention, nil
}

func (f *fakeAPI) Usage(context.Context) (*usageView, error) { return f.usage, f.usageErr }

// TestComputeStatePrecedence lists every combination once: throttled beats
// attention beats working beats idle.
func TestComputeStatePrecedence(t *testing.T) {
	cases := []struct {
		name       string
		throttled  bool
		humanQueue int
		failed     bool
		running    bool
		want       string
	}{
		{"all quiet", false, 0, false, false, "idle"},
		{"running only", false, 0, false, true, "working"},
		{"human queue", false, 1, false, false, "attention"},
		{"human queue while running", false, 1, false, true, "attention"},
		{"recent failure", false, 0, true, false, "attention"},
		{"recent failure while running", false, 0, true, true, "attention"},
		{"throttled", true, 0, false, false, "throttled"},
		{"throttled beats attention", true, 3, true, false, "throttled"},
		{"throttled beats working", true, 0, false, true, "throttled"},
		{"throttled beats everything", true, 3, true, true, "throttled"},
	}
	for _, c := range cases {
		if got := computeState(c.throttled, c.humanQueue, c.failed, c.running); got != c.want {
			t.Errorf("%s: computeState = %q, want %q", c.name, got, c.want)
		}
	}
}

func testNow() time.Time { return time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC) }

func TestGatherCountsAndRunning(t *testing.T) {
	now := testNow()
	api := &fakeAPI{
		queue: []queueItem{
			{Work: workView{ID: "wp1"}, State: "pending"},
			{Work: workView{ID: "wp2"}, State: "pending"},
			{Work: workView{ID: "wb1"}, State: "blocked", Reason: "waiting"},
			{Work: workView{ID: "wd1"}, State: "deferred", Reason: "budget"},
			{Work: workView{ID: "wr1", RoutineName: "inventory", Title: "Inventory"}, State: "running", Targets: []targetView{
				{ID: "t1", Repository: "equitizr", State: "running", StartedAt: now.Add(-143 * time.Second)},
				{ID: "t2", Repository: "forge", State: "succeeded"},
			}},
		},
		tasks: map[string]*taskDetail{
			"wr1": {Work: workView{ID: "wr1"}, Attempts: []attemptView{
				{ID: "a0", TargetID: "t1", Mode: "run", ModelAlias: "haiku"},
				{ID: "a1", TargetID: "t1", Mode: "run", ModelAlias: "sonnet"},
			}},
		},
	}
	st, err := gather(context.Background(), api, now, nil, defaultUI)
	if err != nil {
		t.Fatalf("gather: %v", err)
	}
	if st.Queued != 2 || st.Blocked != 1 || st.Deferred != 1 {
		t.Fatalf("counts = %d/%d/%d, want 2/1/1", st.Queued, st.Blocked, st.Deferred)
	}
	if len(st.Running) != 1 {
		t.Fatalf("running = %d items, want 1 (only the active target)", len(st.Running))
	}
	r := st.Running[0]
	if r.Work != "wr1" || r.Title != "Inventory" || r.Routine != "inventory" || r.Repo != "equitizr" {
		t.Errorf("running item identity = %+v", r)
	}
	if r.Mode != "run" || r.Model != "sonnet" {
		t.Errorf("running item mode/model = %q/%q, want run/sonnet (latest attempt)", r.Mode, r.Model)
	}
	if r.ElapsedS != 143 {
		t.Errorf("elapsed_s = %d, want 143", r.ElapsedS)
	}
	if want := defaultUI + "/tasks/wr1"; r.URL != want {
		t.Errorf("url = %q, want %q", r.URL, want)
	}
	if st.State != "working" {
		t.Errorf("state = %q, want working", st.State)
	}
	if st.Usage != nil {
		t.Errorf("usage block present without a usage response")
	}
}

func TestGatherRunningSurvivesDetailError(t *testing.T) {
	now := testNow()
	api := &fakeAPI{queue: []queueItem{
		{Work: workView{ID: "wr1", Title: "T"}, State: "running", Targets: []targetView{
			{ID: "t1", Repository: "equitizr", State: "preparing", StartedAt: now.Add(-5 * time.Second)},
		}},
	}}
	st, err := gather(context.Background(), api, now, nil, defaultUI)
	if err != nil {
		t.Fatalf("gather: %v", err)
	}
	if len(st.Running) != 1 || st.Running[0].Repo != "equitizr" || st.Running[0].Mode != "" {
		t.Fatalf("running = %+v, want one degraded entry", st.Running)
	}
	if st.State != "working" {
		t.Errorf("state = %q, want working", st.State)
	}
}

func TestGatherAttentionAndUsage(t *testing.T) {
	now := testNow()
	api := &fakeAPI{
		attention: &attentionView{
			Questions: []questionView{{ID: "q1", WorkID: "w9", Text: "Which README?\ndetails", AskedAt: now.Add(-320 * time.Second)}},
			Proposals: []proposalView{{ID: "p1", Kind: "routine", Target: "inventory", CreatedAt: now.Add(-60 * time.Second)}},
		},
		usage: &usageView{},
	}
	api.usage.Usage.FiveHour = windowView{Utilization: 0.41, ResetsAt: now.Add(5400 * time.Second)}
	api.usage.Usage.SevenDay = windowView{Utilization: 0.22, ResetsAt: now.Add(300000 * time.Second)}
	api.usage.Config.FiveHourTarget, api.usage.Config.SevenDayTarget = 0.9, 0.9
	api.usage.Config.FiveHourHardStop, api.usage.Config.SevenDayHardStop = 0.98, 0.98

	st, err := gather(context.Background(), api, now, nil, defaultUI)
	if err != nil {
		t.Fatalf("gather: %v", err)
	}
	if st.HumanQueue.Questions != 1 || st.HumanQueue.Proposals != 1 || st.HumanQueue.Verifications != 0 {
		t.Fatalf("human queue counts = %+v", st.HumanQueue)
	}
	if len(st.HumanQueue.Items) != 2 {
		t.Fatalf("human queue items = %d, want 2", len(st.HumanQueue.Items))
	}
	q := st.HumanQueue.Items[0]
	if q.Kind != "question" || q.Title != "Which README?" || q.AgeS != 320 || q.URL != defaultUI+"/tasks/w9" {
		t.Errorf("question item = %+v", q)
	}
	if st.State != "attention" {
		t.Errorf("state = %q, want attention", st.State)
	}
	if st.Usage == nil {
		t.Fatal("usage block missing")
	}
	if st.Usage.FiveHour.Utilization != 0.41 || st.Usage.FiveHour.ResetsInS != 5400 || st.Usage.FiveHour.Target != 0.9 {
		t.Errorf("five_hour = %+v", st.Usage.FiveHour)
	}
	if st.Usage.SevenDay.ResetsInS != 300000 {
		t.Errorf("seven_day resets_in_s = %d", st.Usage.SevenDay.ResetsInS)
	}
}

func TestGatherThrottledByUsageHardStop(t *testing.T) {
	now := testNow()
	api := &fakeAPI{
		queue:     []queueItem{{Work: workView{ID: "wr1"}, State: "running", Targets: []targetView{{ID: "t1", Repository: "r", State: "running"}}}},
		attention: &attentionView{Questions: []questionView{{ID: "q1", WorkID: "w9", Text: "x", AskedAt: now}}},
		usage:     &usageView{},
	}
	api.usage.Usage.FiveHour = windowView{Utilization: 0.99}
	api.usage.Config.FiveHourHardStop = 0.98
	st, err := gather(context.Background(), api, now, nil, defaultUI)
	if err != nil {
		t.Fatalf("gather: %v", err)
	}
	if st.State != "throttled" {
		t.Errorf("state = %q, want throttled (hard stop beats attention and working)", st.State)
	}
}

func TestGatherThrottledByDeferredReason(t *testing.T) {
	// No usage:read needed: the queue's deferred reason carries the hard stop.
	api := &fakeAPI{queue: []queueItem{{Work: workView{ID: "w1"}, State: "deferred", Reason: "hard_stop:five_hour"}}}
	st, err := gather(context.Background(), api, testNow(), nil, defaultUI)
	if err != nil {
		t.Fatalf("gather: %v", err)
	}
	if st.State != "throttled" {
		t.Errorf("state = %q, want throttled", st.State)
	}
	if st.Deferred != 1 {
		t.Errorf("deferred = %d, want 1", st.Deferred)
	}
}

func TestGatherLastFailure(t *testing.T) {
	now := testNow()
	failures := []failureRecord{
		{JournalID: 1, Work: "w1", Routine: "touch", Reason: "timeout", At: now.Add(-50 * time.Minute)},
		{JournalID: 2, Work: "w2", Routine: "touch", Reason: "nonzero_exit", At: now.Add(-30 * time.Minute)},
	}
	st, err := gather(context.Background(), &fakeAPI{}, now, failures, defaultUI)
	if err != nil {
		t.Fatalf("gather: %v", err)
	}
	if st.State != "attention" {
		t.Errorf("state = %q, want attention (failure in the last hour)", st.State)
	}
	if st.LastFailure == nil || st.LastFailure.Work != "w2" || st.LastFailure.Reason != "nonzero_exit" {
		t.Fatalf("last_failure = %+v, want the newest record", st.LastFailure)
	}
	if st.LastFailure.TS != "2026-08-30T11:30:00Z" {
		t.Errorf("last_failure ts = %q", st.LastFailure.TS)
	}
}

func TestGatherIdle(t *testing.T) {
	st, err := gather(context.Background(), &fakeAPI{}, testNow(), nil, defaultUI)
	if err != nil {
		t.Fatalf("gather: %v", err)
	}
	if st.State != "idle" {
		t.Errorf("state = %q, want idle", st.State)
	}
	if st.Running == nil {
		t.Error("running must marshal as [], not null")
	}
}

// TestStatusJSONShape pins the wire format: exactly the docs/PLUGINS.md schema,
// in its published key order.
func TestStatusJSONShape(t *testing.T) {
	st := &status{
		Schema: 1, TS: "2026-08-30T12:00:00Z", Daemon: "ok", State: "working",
		Running: []runningItem{{Work: "w1", Title: "Inventory", Routine: "inventory", Repo: "equitizr",
			Mode: "run", ElapsedS: 143, Model: "sonnet", URL: "http://127.0.0.1:7340/tasks/w1"}},
		Queued: 3, Blocked: 1, Deferred: 2,
		HumanQueue: humanQueue{Questions: 1, Proposals: 2, Verifications: 0, Items: []humanItem{
			{Kind: "question", ID: "q1", Title: "Which README?", AgeS: 320, URL: "http://127.0.0.1:7340/tasks/w1"}}},
		Usage: &usageBlock{
			FiveHour: usageWindow{Utilization: 0.41, ResetsInS: 5400, Target: 0.9},
			SevenDay: usageWindow{Utilization: 0.22, ResetsInS: 300000, Target: 0.9}},
		LastFailure: &lastFailure{Work: "w2", Routine: "touch", Reason: "nonzero_exit",
			TS: "2026-08-30T11:30:00Z", URL: "http://127.0.0.1:7340/tasks/w2"},
		UI: "http://127.0.0.1:7340",
	}
	got, err := json.Marshal(st)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	want := `{"schema":1,"ts":"2026-08-30T12:00:00Z","daemon":"ok","state":"working",` +
		`"running":[{"work":"w1","title":"Inventory","routine":"inventory","repo":"equitizr","mode":"run","elapsed_s":143,"model":"sonnet","url":"http://127.0.0.1:7340/tasks/w1"}],` +
		`"queued":3,"blocked":1,"deferred":2,` +
		`"human_queue":{"questions":1,"proposals":2,"verifications":0,"items":[{"kind":"question","id":"q1","title":"Which README?","age_s":320,"url":"http://127.0.0.1:7340/tasks/w1"}]},` +
		`"usage":{"five_hour":{"utilization":0.41,"resets_in_s":5400,"target":0.9},"seven_day":{"utilization":0.22,"resets_in_s":300000,"target":0.9}},` +
		`"last_failure":{"work":"w2","routine":"touch","reason":"nonzero_exit","ts":"2026-08-30T11:30:00Z","url":"http://127.0.0.1:7340/tasks/w2"},` +
		`"ui":"http://127.0.0.1:7340"}`
	if string(got) != want {
		t.Errorf("status JSON:\n got %s\nwant %s", got, want)
	}
}

func TestFailureRing(t *testing.T) {
	now := testNow()
	var r failureRing
	r.add(failureRecord{JournalID: 1, Work: "w1", At: now.Add(-2 * time.Hour)})
	r.add(failureRecord{JournalID: 2, Work: "w2", At: now.Add(-10 * time.Minute)})
	r.add(failureRecord{JournalID: 2, Work: "w2", At: now.Add(-10 * time.Minute)}) // replayed entry
	got := r.recent(now, time.Hour)
	if len(got) != 1 || got[0].Work != "w2" {
		t.Fatalf("recent = %+v, want just w2 once", got)
	}
	for i := range ringCap + 10 {
		r.add(failureRecord{JournalID: int64(100 + i), At: now})
	}
	r.mu.Lock()
	n := len(r.entries)
	r.mu.Unlock()
	if n != ringCap {
		t.Errorf("ring holds %d entries, want cap %d", n, ringCap)
	}
}

func TestRelevantKinds(t *testing.T) {
	for _, k := range []string{"work.created", "target.transition", "question.asked", "proposal.decided", "daemon.draining", "budget.reset"} {
		if !relevant(k) {
			t.Errorf("relevant(%q) = false", k)
		}
	}
	for _, k := range []string{"attempt.completed", "plugin.enabled", "workx"} {
		if relevant(k) {
			t.Errorf("relevant(%q) = true", k)
		}
	}
}
