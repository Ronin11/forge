package main

import (
	"context"
	"errors"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// fakeGitHub implements gitHub in memory; nothing is ever exec'd.
type fakeGitHub struct {
	issues  map[string][]issue // by repo slug
	listErr error

	comments []commentCall
	labels   []labelCall
}

type commentCall struct {
	repo   string
	number int
	body   string
}

type labelCall struct {
	repo   string
	number int
	label  string
}

func (f *fakeGitHub) ListIssues(_ context.Context, repo, _ string) ([]issue, error) {
	if f.listErr != nil {
		return nil, f.listErr
	}
	return f.issues[repo], nil
}

func (f *fakeGitHub) Comment(_ context.Context, repo string, number int, body string) error {
	f.comments = append(f.comments, commentCall{repo, number, body})
	return nil
}

func (f *fakeGitHub) Label(_ context.Context, repo string, number int, label string) error {
	f.labels = append(f.labels, labelCall{repo, number, label})
	return nil
}

// fakeForge implements forgeAPI in memory; no daemon is spun up.
type fakeForge struct {
	created   []createTaskRequest
	annotated []annotateCall
	nextID    string
	createErr error
	status    map[string]*taskStatus // by workID
}

type annotateCall struct {
	workID string
	refs   []externalRef
}

func (f *fakeForge) CreateTask(_ context.Context, req createTaskRequest) (string, error) {
	if f.createErr != nil {
		return "", f.createErr
	}
	f.created = append(f.created, req)
	id := f.nextID
	if id == "" {
		id = "work00000001"
	}
	return id, nil
}

func (f *fakeForge) Annotate(_ context.Context, workID string, refs []externalRef) error {
	f.annotated = append(f.annotated, annotateCall{workID, refs})
	return nil
}

func (f *fakeForge) TaskState(_ context.Context, workID string) (*taskStatus, error) {
	if s, ok := f.status[workID]; ok {
		return s, nil
	}
	return nil, errors.New("not found")
}

func testPoller(t *testing.T, gh gitHub, api forgeAPI, cfg config) *poller {
	t.Helper()
	statePath := filepath.Join(t.TempDir(), "state.json")
	return &poller{
		api: api,
		gh:  gh,
		st:  loadState(statePath, discardLogger()),
		cfg: cfg,
		log: discardLogger(),
		now: func() time.Time { return time.Date(2026, 8, 31, 12, 0, 0, 0, time.UTC) },
		ui:  defaultUI,
	}
}

func oneRepoConfig() config {
	return config{
		PollSeconds: 60,
		Repos: []repoConfig{{
			GitHub: "Ronin11/forge", Forge: "forge", Label: "forge",
			Mode: "implement", Class: "normal", Autonomy: "auto",
			Integrate: false, Comment: true, DoneLabel: "forge:done",
		}},
	}
}

func TestIngestCycle(t *testing.T) {
	gh := &fakeGitHub{issues: map[string][]issue{
		"Ronin11/forge": {
			{Number: 12, Title: "crash on start", Body: "it dies", URL: "https://github.com/Ronin11/forge/issues/12"},
			{Number: 13, Title: "already seen", Body: "old", URL: "https://github.com/Ronin11/forge/issues/13"},
		},
	}}
	api := &fakeForge{nextID: "workaaaabbbbcccc"}
	p := testPoller(t, gh, api, oneRepoConfig())

	// Pre-seed issue 13 as already tracked; it must not be re-created.
	seen := issueKey("Ronin11/forge", 13)
	if err := p.st.put(seen, tracked{WorkID: "old000", Number: 13, GitHub: "Ronin11/forge", Status: statusIngested}); err != nil {
		t.Fatal(err)
	}

	p.ingest(context.Background())

	if len(api.created) != 1 {
		t.Fatalf("created %d tasks, want 1", len(api.created))
	}
	req := api.created[0]
	if len(req.Repositories) != 1 || req.Repositories[0] != "forge" {
		t.Errorf("repositories = %v", req.Repositories)
	}
	if req.Mode != "implement" {
		t.Errorf("mode = %q", req.Mode)
	}
	if !strings.Contains(req.Prompt, "GitHub issue #12 — crash on start") ||
		!strings.Contains(req.Prompt, "it dies") ||
		!strings.Contains(req.Prompt, "(from https://github.com/Ronin11/forge/issues/12)") ||
		!strings.Contains(req.Prompt, "acceptance criteria") {
		t.Errorf("prompt = %q", req.Prompt)
	}
	if !strings.Contains(req.Title, "issue #12:") {
		t.Errorf("title = %q", req.Title)
	}

	// Exactly one external-ref, id "<github>#<number>".
	if len(api.annotated) != 1 {
		t.Fatalf("annotated %d, want 1", len(api.annotated))
	}
	ref := api.annotated[0].refs[0]
	if ref.Kind != "issue" || ref.ID != "Ronin11/forge#12" || ref.Label != "#12" || ref.URL == "" {
		t.Errorf("external ref = %+v", ref)
	}
	if api.annotated[0].workID != "workaaaabbbbcccc" {
		t.Errorf("annotated workID = %q", api.annotated[0].workID)
	}

	// Exactly one pickup comment, on issue 12.
	if len(gh.comments) != 1 || gh.comments[0].number != 12 {
		t.Fatalf("comments = %+v", gh.comments)
	}
	if !strings.Contains(gh.comments[0].body, "picked this up") || !strings.Contains(gh.comments[0].body, shortID("workaaaabbbbcccc")) {
		t.Errorf("pickup comment = %q", gh.comments[0].body)
	}

	// Issue 12 is now tracked; 13 still tracked (never re-created).
	if !p.st.has(issueKey("Ronin11/forge", 12)) {
		t.Error("issue 12 not recorded")
	}

	// A second ingest creates nothing new.
	p.ingest(context.Background())
	if len(api.created) != 1 {
		t.Errorf("second ingest re-created tasks: %d total", len(api.created))
	}
}

func TestTerminalTransition(t *testing.T) {
	gh := &fakeGitHub{issues: map[string][]issue{
		"Ronin11/forge": {{Number: 12, Title: "fix me", Body: "b", URL: "https://github.com/Ronin11/forge/issues/12"}},
	}}
	api := &fakeForge{
		nextID: "work12341234",
		status: map[string]*taskStatus{
			"work12341234": {State: "succeeded", Targets: []target{{State: "succeeded", Branch: "forge/issue-12"}}},
		},
	}
	p := testPoller(t, gh, api, oneRepoConfig())

	p.ingest(context.Background())
	// One pickup comment so far.
	if len(gh.comments) != 1 {
		t.Fatalf("after ingest, comments = %d", len(gh.comments))
	}

	p.reconcile(context.Background())

	// Exactly one outcome comment (total now 2: pickup + outcome).
	if len(gh.comments) != 2 {
		t.Fatalf("after reconcile, comments = %d, want 2", len(gh.comments))
	}
	outcome := gh.comments[1].body
	if !strings.Contains(outcome, "forge/issue-12") {
		t.Errorf("outcome missing branch: %q", outcome)
	}
	if !strings.Contains(outcome, defaultUI+"/tasks/work12341234") {
		t.Errorf("outcome missing task URL: %q", outcome)
	}
	if !strings.Contains(outcome, "✅") {
		t.Errorf("succeeded outcome should be a check: %q", outcome)
	}

	// Best-effort done label applied.
	if len(gh.labels) != 1 || gh.labels[0].label != "forge:done" || gh.labels[0].number != 12 {
		t.Errorf("done label = %+v", gh.labels)
	}

	// State marked done.
	got := p.st.entries[issueKey("Ronin11/forge", 12)]
	if got.Status != statusDone || !got.DoneCommented {
		t.Errorf("entry not marked done: %+v", got)
	}

	// A second reconcile does NOT comment again.
	p.reconcile(context.Background())
	if len(gh.comments) != 2 {
		t.Errorf("second reconcile re-commented: %d comments", len(gh.comments))
	}
}

func TestCreateErrorLeavesIssueUntracked(t *testing.T) {
	gh := &fakeGitHub{issues: map[string][]issue{
		"Ronin11/forge": {{Number: 12, Title: "x", Body: "y", URL: "u"}},
	}}
	api := &fakeForge{createErr: errors.New("daemon down")}
	p := testPoller(t, gh, api, oneRepoConfig())

	p.ingest(context.Background())

	// Nothing recorded, no comment, no annotate — so a later cycle retries.
	if p.st.has(issueKey("Ronin11/forge", 12)) {
		t.Error("issue recorded despite create failure")
	}
	if len(gh.comments) != 0 {
		t.Errorf("commented despite create failure: %+v", gh.comments)
	}
	if len(api.annotated) != 0 {
		t.Errorf("annotated despite create failure")
	}

	// Recovery: the next cycle succeeds and records it.
	api.createErr = nil
	api.nextID = "recovered001"
	p.ingest(context.Background())
	if !p.st.has(issueKey("Ronin11/forge", 12)) {
		t.Error("issue not recorded after recovery")
	}
}

func TestOutcomeCommentByState(t *testing.T) {
	cases := []struct {
		state  string
		branch string
		want   []string
	}{
		{"succeeded", "b1", []string{"✅", "b1", "succeeded"}},
		{"merged", "b2", []string{"✅", "b2", "merged"}},
		{"unverified", "b3", []string{"⚠️", "b3", "couldn't verify"}},
		{"failed", "b4", []string{"❌", "failed"}},
		{"partial", "b5", []string{"❌", "partial"}},
		{"cancelled", "b6", []string{"❌", "cancelled"}},
		{"conflict", "b7", []string{"❌", "conflict"}},
	}
	for _, tc := range cases {
		st := &taskStatus{State: tc.state, Targets: []target{{Branch: tc.branch}}}
		body := outcomeComment(st, defaultUI, "wid")
		for _, w := range tc.want {
			if !strings.Contains(body, w) {
				t.Errorf("state %s: body %q missing %q", tc.state, body, w)
			}
		}
		if !strings.Contains(body, defaultUI+"/tasks/wid") {
			t.Errorf("state %s: missing task URL: %q", tc.state, body)
		}
	}
}

func TestPickBranchFallsBackToAttempt(t *testing.T) {
	st := &taskStatus{
		Targets:  []target{{Branch: ""}},
		Attempts: []attempt{{Branch: "first"}, {Branch: "last"}},
	}
	if b := pickBranch(st); b != "last" {
		t.Errorf("pickBranch = %q, want last attempt", b)
	}
}

func TestResultSummaryIncluded(t *testing.T) {
	st := &taskStatus{
		State:   "succeeded",
		Targets: []target{{Branch: "b"}},
		Attempts: []attempt{{Branch: "b", Result: struct {
			Summary string `json:"summary"`
		}{Summary: "changed the parser"}}},
	}
	body := outcomeComment(st, defaultUI, "w")
	if !strings.Contains(body, "changed the parser") {
		t.Errorf("summary not included: %q", body)
	}
}

func TestNonTerminalNotCommented(t *testing.T) {
	gh := &fakeGitHub{issues: map[string][]issue{
		"Ronin11/forge": {{Number: 12, Title: "x", Body: "y", URL: "u"}},
	}}
	api := &fakeForge{
		nextID: "runningwork1",
		status: map[string]*taskStatus{"runningwork1": {State: "running"}},
	}
	p := testPoller(t, gh, api, oneRepoConfig())
	p.ingest(context.Background())
	p.reconcile(context.Background())
	// Only the pickup comment; no outcome for a running task.
	if len(gh.comments) != 1 {
		t.Errorf("running task commented outcome: %+v", gh.comments)
	}
	if p.st.entries[issueKey("Ronin11/forge", 12)].Status != statusIngested {
		t.Error("running task marked done")
	}
}
