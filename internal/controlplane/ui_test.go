package controlplane

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

	"forge/internal/model"
	"forge/internal/protocol"
	"forge/internal/store"
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
		if err := tx.CreateProposal(ctx, &store.Proposal{Source: "manual", Kind: model.ProposalProcess, Target: "routine:inventory", After: []byte(`{"priority":40}`),
			Rationale: "Trim the timeout", VerificationPlan: "watch the next 5 runs"}); err != nil {
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
			Context: json.RawMessage(`{"actions":[{"label":"Open the doc","url":"/kb/setup-guide"},{"label":"Send test toast","trigger":"notify_test"},{"label":"evil","url":"https://evil.example"}]}`)})
		return err
	}); err != nil {
		t.Fatal(err)
	}
	ui, err := NewUI(st, slog.New(slog.DiscardHandler), time.Now)
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(ui.Handler())
	defer srv.Close()
	for path, want := range map[string][]string{
		"/":                     {"Dashboard", "laptop", "inventory", `data-searchbar="client"`, "data-chat-toggle", `data-chat-route="explore"`},
		"/tasks":                {"inventory run", "equitizr", "running", `data-searchbar="client"`, `data-f-repo="equitizr`, `data-keys="repo,state,routine,class"`},
		"/tasks/" + work.ID:     {"inventory@1", "claimed", "fetch", "1ms"},
		"/routines":             {"inventory", "list files", "data-routine-new", `data-routine-edit="inventory"`, `data-action-post="/api/v1/routines/inventory/run"`},
		"/workflows":            {"nightly", "scan → fix", "data-workflow-new", `data-workflow-edit="nightly"`, `data-action-post="/api/v1/workflows/nightly/run"`, `datalist id="routine-names"`, `<option value="inventory">`},
		"/system":               {"laptop", "github.com/x/equitizr", "sandbox=ready", "data-chat-toggle"},
		"/repos/equitizr":       {"equitizr", "Location", "Pause", "Checks"},
		"/repos/does-not-exist": {"not found"},
		"/static/style.css":     {"nav.top"},
		"/static/app.js":        {"data-timeline"},
		"/tasks/does-not-exist": {"not found"},
		"/queue":                {"Queue", "inventory run", "data-queue", `data-searchbar="client"`, `data-f-repo="equitizr`},
		"/stats?since=1d":       {"Stats", "window 1d", "2 proposed", `data-searchbar="client"`},
		"/attention": {"Human Queue", "Trim the timeout", "forge proposal approve", `data-searchbar="client"`, `data-f-type="proposal"`,
			"Verify notify click-routing", `<a href="/kb/setup-guide">Open the doc →</a>`,
			`<button data-action-post="/api/v1/notify/test">Send test toast</button>`,
			`<a href="/kb/setup-guide">kb:setup-guide</a>`},
		"/proposals": {"Proposals", "routine:inventory", "Trim the timeout", "Approve", `data-searchbar="client"`, `data-f-status="proposed"`},
		"/kb":        {"Knowledge", `data-searchbar="server"`, `data-keys="tag,type"`},
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
