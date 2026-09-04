package ui

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

func TestUIPagesRender(t *testing.T) {
	ctx := context.Background()
	st, err := store.Open(ctx, filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	}()
	workerID := "0123456789abcdef0123456789abcdef"
	var work *store.Work
	var prop *store.Proposal
	if err := st.Write(ctx, func(tx *store.Tx) error {
		if err := tx.EnsureProject(ctx, "default"); err != nil {
			return err
		}
		if err := tx.Register(ctx, protocol.RegisterRequest{WorkerID: workerID, Name: "laptop", Version: "test", MaxConcurrent: 2, Executors: []string{"claude-code"}, Capabilities: map[string]string{"sandbox": "ready"},
			Repositories: []protocol.Repository{{Name: "equitizr", Path: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr"}}}); err != nil {
			return err
		}
		r := &store.Routine{Name: "inventory", Mode: "run", Prompt: "list files", Repositories: []string{"equitizr"}, Model: "haiku", TimeoutSeconds: 300}
		if err := tx.CreateRoutine(ctx, r); err != nil {
			return err
		}
		prop = &store.Proposal{Source: "manual", Kind: model.ProposalProcess, Target: "routine:inventory", After: []byte(`{"priority":40}`),
			Rationale: "Trim the timeout", VerificationPlan: "watch the next 5 runs"}
		if err := tx.CreateProposal(ctx, prop); err != nil {
			return err
		}
		if err := tx.CreateWorkflow(ctx, &store.Workflow{Name: "nightly", Steps: []store.WorkflowStep{{Name: "scan", Routine: "inventory"}, {Name: "fix", Routine: "inventory"}}}); err != nil {
			return err
		}
		work = &store.Work{RoutineID: r.ID, RoutineName: "inventory", Generation: 1, Title: "inventory run", Trigger: model.TriggerManual, Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassInteractive, Autonomy: model.AutonomyAuto}
		targets, err := tx.CreateWork(ctx, work, []string{"equitizr"}, nil)
		if err != nil {
			return err
		}
		a, err := tx.Claim(ctx, store.ClaimParams{TargetID: targets[0].ID, WorkerID: workerID, ClaimRequestID: "r1", LeaseToken: "l", MCPToken: "m", Executor: "claude-code", Model: "claude-haiku-4-5", ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAuto})
		if err != nil {
			return err
		}
		if _, err = tx.InsertEvents(ctx, a.ID, protocol.SourceWorker, []protocol.Event{{Seq: 0, Time: time.Now(), Kind: protocol.KindSpanEnd, SpanID: "fetch", Name: "fetch", DurationUS: 1200, ElapsedUS: 1300}}); err != nil {
			return err
		}
		if err := tx.CreateProposal(ctx, &store.Proposal{Source: "manual", Kind: model.ProposalDoc, Target: "kb:setup-guide", After: []byte(`{"content":"agent-written"}`),
			Rationale: "Document the setup", VerificationPlan: "human reads the note"}); err != nil {
			return err
		}
		_, err = tx.CreateQuestion(ctx, a, protocol.QuestionRequest{Text: "Verify notify click-routing",
			Context: json.RawMessage(`{"actions":[{"label":"Open the doc","url":"/kb/setup-guide"},{"label":"Send test toast","rpc":"notify.test"},{"label":"evil","url":"https://evil.example"}]}`)})
		return err
	}); err != nil {
		t.Fatal(err)
	}
	ui, err := NewUI(st, slog.New(slog.DiscardHandler), time.Now, nil)
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(ui.Handler())
	defer srv.Close()
	for path, want := range map[string][]string{
		"/":                              {"Dashboard", "laptop", "inventory", `data-searchbar="client"`, "data-chat-toggle", `data-chat-route="explore"`},
		"/tasks":                         {"New task", "data-task-scope", ">Open<", ">Closed<", `data-searchbar="client"`, "data-task-more"},
		"/tasks?scope=all":               {"inventory run", "equitizr", "running", `data-searchbar="client"`, `data-f-repo="equitizr`, `data-keys="repo,state,routine,class"`, "New task", `data-task-dialog`, `name="max_turns"`, `name="timeout_seconds"`},
		"/tasks/rows?scope=all&offset=0": {`data-href="/tasks/`, "equitizr", "<tr"},
		"/tasks/" + work.ID:              {"inventory@1", "claimed", "fetch", "1ms"},
		"/routines":                      {"Prompts", "inventory", "data-routine-new", `data-sel="routine:inventory"`, "personas/", "fragments/", "routines/", "prompts.js"},
		"/workflows":                     {"nightly", "scan → fix", `href="/workflows/new"`, `href="/workflows/nightly/edit"`, `data-action-post="/api/v1/workflows/nightly/run"`, `href="/workflows/nightly/runs"`},
		"/workflows/new":                 {"New workflow", "data-graph-editor", "data-gv-stage", "graph.js"},
		"/workflows/nightly/edit":        {"Edit nightly", `data-workflow-name="nightly"`, "data-graph-editor"},
		"/workflows/nightly/runs":        {"nightly · Runs", "No runs yet"},
		"/system":                        {"laptop", "github.com/x/equitizr", "sandbox=ready", "data-chat-toggle", "Settings", ">General<", ">Plugins<"},
		"/settings":                      {"Settings", "Daemon", "Logging", "data-settings-general", "data-loglevel-form", `data-action-post="/api/v1/backup"`, ">General<"},
		"/settings/plugins":              {"Plugins", "data-plugin-install-form", "Install a plugin", "No plugins installed"},
		"/repos/equitizr":                {"equitizr", "Location", "Pause", "Checks", "data-app-card", "data-app-action=\"start\""},
		"/repos":                         {"Repos", "data-repo-add", `data-repo-archive="equitizr"`, "equitizr", `data-searchbar="client"`},
		"/repos/does-not-exist":          {"not found"},
		"/static/style.css":              {"nav.top"},
		"/static/app.js":                 {"data-timeline"},
		"/tasks/does-not-exist":          {"not found"},
		"/queue":                         {"Queue", "inventory run", "data-queue", `data-searchbar="client"`, `data-f-repo="equitizr`},
		"/stats?since=1d":                {"Stats", "window 1d", "2 proposed", `data-searchbar="client"`},
		"/attention": {"Human Queue", "Trim the timeout", "forge proposal approve", `data-searchbar="client"`, `data-f-type="proposal"`,
			"Verify notify click-routing", `<a href="/kb/setup-guide">Open the doc →</a>`,
			`<button data-rpc="notify.test">Send test toast</button>`,
			`<a href="/kb/setup-guide">kb:setup-guide</a>`},
		"/proposals":            {"Proposals", "routine:inventory", "Trim the timeout", "Approve", `data-searchbar="client"`, `data-f-status="proposed"`},
		"/proposals/" + prop.ID: {"Proposal", "Trim the timeout", "Verification plan", "watch the next 5 runs", "Decision history", "proposal.created", "<h2>After</h2>", "priority"},
		"/kb":                   {"Knowledge", `data-searchbar="server"`, `data-keys="tag,type"`},
	} {
		resp, err := http.Get(srv.URL + path)
		if err != nil {
			t.Fatal(err)
		}
		body, err := io.ReadAll(resp.Body)
		if err != nil {
			t.Fatal(err)
		}
		if err := resp.Body.Close(); err != nil {
			t.Fatal(err)
		}
		for _, w := range want {
			if !strings.Contains(string(body), w) {
				t.Errorf("%s: missing %q (status %d)\n%s", path, w, resp.StatusCode, truncateBody(body))
			}
		}
	}
}

func TestParseKbQuery(t *testing.T) {
	for _, tc := range []struct {
		q, text, tag, typ string
	}{
		{"", "", "", ""},
		{"plain words", "plain words", "", ""},
		{"tag:retro", "", "retro", ""},
		{"type:audit budget", "budget", "", "audit"},
		{`tag:"ui-test" Type:brief drag`, "drag", "ui-test", "brief"},
		{"tag:a tag:b", "", "b", ""},                             // last one wins
		{"http://x other:thing", "http://x other:thing", "", ""}, // unknown keys stay text
	} {
		text, tag, typ := parseKbQuery(tc.q)
		if text != tc.text || tag != tc.tag || typ != tc.typ {
			t.Errorf("parseKbQuery(%q) = (%q, %q, %q), want (%q, %q, %q)", tc.q, text, tag, typ, tc.text, tc.tag, tc.typ)
		}
	}
}

func truncateBody(b []byte) string {
	if len(b) > 1500 {
		return string(b[:1500]) + "…"
	}
	return string(b)
}

// TestProposalsScope: the page defaults to open proposals — decided ones live
// behind the Closed tab, so a long tail of rejected/applied rows never buries
// the one waiting on a human.
func TestProposalsScope(t *testing.T) {
	ctx := context.Background()
	st, err := store.Open(ctx, filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	}()
	if err := st.Write(ctx, func(tx *store.Tx) error {
		open := &store.Proposal{Source: "manual", Kind: model.ProposalDoc, Target: "kb:still-open", After: []byte(`{}`), Rationale: "undecided rationale", VerificationPlan: "human reads it"}
		if err := tx.CreateProposal(ctx, open); err != nil {
			return err
		}
		done := &store.Proposal{Source: "manual", Kind: model.ProposalDoc, Target: "kb:already-rejected", After: []byte(`{}`), Rationale: "decided rationale", VerificationPlan: "human reads it"}
		if err := tx.CreateProposal(ctx, done); err != nil {
			return err
		}
		_, err := tx.DecideProposal(ctx, done.ID, model.ProposalRejected, "human")
		return err
	}); err != nil {
		t.Fatal(err)
	}
	ui, err := NewUI(st, slog.New(slog.DiscardHandler), time.Now, nil)
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(ui.Handler())
	defer srv.Close()

	for path, want := range map[string]struct{ has, hasNot []string }{
		"/proposals":              {has: []string{"kb:still-open", ">Open 1<", ">Closed 1<"}, hasNot: []string{"kb:already-rejected"}},
		"/proposals?scope=open":   {has: []string{"kb:still-open"}, hasNot: []string{"kb:already-rejected"}},
		"/proposals?scope=closed": {has: []string{"kb:already-rejected"}, hasNot: []string{"kb:still-open"}},
		"/proposals?scope=all":    {has: []string{"kb:still-open", "kb:already-rejected"}},
		"/proposals?scope=bogus":  {has: []string{"kb:still-open"}, hasNot: []string{"kb:already-rejected"}},
	} {
		resp, err := http.Get(srv.URL + path)
		if err != nil {
			t.Fatal(err)
		}
		body, err := io.ReadAll(resp.Body)
		if err != nil {
			t.Fatal(err)
		}
		if err := resp.Body.Close(); err != nil {
			t.Fatal(err)
		}
		for _, w := range want.has {
			if !strings.Contains(string(body), w) {
				t.Errorf("GET %s: missing %q", path, w)
			}
		}
		for _, w := range want.hasNot {
			if strings.Contains(string(body), w) {
				t.Errorf("GET %s: unexpectedly contains %q", path, w)
			}
		}
	}
}
