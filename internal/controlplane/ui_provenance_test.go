package controlplane

import (
	"context"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/store"
)

// TestUIProvenanceViews renders the task-detail strip, the /work tree, and the
// canonicalizing redirect against a seeded root → two children tree.
func TestUIProvenanceViews(t *testing.T) {
	ctx := context.Background()
	st, err := store.Open(ctx, t.TempDir()+"/forge.sqlite3", store.Options{Clock: func() time.Time { return time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC) }})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})

	var root, planned, verified *store.Work
	if err := st.Write(ctx, func(tx *store.Tx) error {
		if err := tx.EnsureProject(ctx, "default"); err != nil {
			return err
		}
		if err := tx.Register(ctx, protocol.RegisterRequest{WorkerID: testWorkerID, Name: "laptop", Version: "t", MaxConcurrent: 2, Executors: []string{"claude-code"},
			Repositories: []protocol.Repository{{Name: "equitizr", Path: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr"}}}); err != nil {
			return err
		}
		root = &store.Work{RoutineName: "plan", Title: "the plan", Trigger: model.TriggerManual, Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto}
		if _, err := tx.CreateWork(ctx, root, []string{"equitizr"}, nil); err != nil {
			return err
		}
		planned = &store.Work{RoutineName: "plan-task", Title: "planned child", Trigger: model.TriggerDependency, Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto, CausedByWorkID: root.ID, Cause: model.CausePlanTask}
		if _, err := tx.CreateWork(ctx, planned, []string{"equitizr"}, nil); err != nil {
			return err
		}
		verified = &store.Work{RoutineName: "verify", Title: "verify child", Trigger: model.TriggerDependency, Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto, CausedByWorkID: planned.ID, Cause: model.CauseVerify}
		if _, err := tx.CreateWork(ctx, verified, []string{"equitizr"}, nil); err != nil {
			return err
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}

	ui, err := NewUI(st, slog.New(slog.DiscardHandler), time.Now)
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(ui.Handler())
	defer srv.Close()

	get := func(path string) string {
		t.Helper()
		resp, err := http.Get(srv.URL + path)
		if err != nil {
			t.Fatal(err)
		}
		b, err := io.ReadAll(resp.Body)
		if err != nil {
			t.Fatal(err)
		}
		if err := resp.Body.Close(); err != nil {
			t.Fatal(err)
		}
		return string(b)
	}

	// A middle child's task page: breadcrumb up to the root, backward "planned
	// by", forward "verify", and the View work link to the root.
	child := get("/tasks/" + planned.ID)
	for _, want := range []string{
		`class="prov`, "planned by " + root.ID[:8], "verify " + verified.ID[:8],
		`href="/work/` + root.ID, "View work",
	} {
		if !strings.Contains(child, want) {
			t.Errorf("/tasks/%s: missing %q", planned.ID[:8], want)
		}
	}

	// The root's own task page shows the strip too (it has children).
	if r := get("/tasks/" + root.ID); !strings.Contains(r, `class="prov`) {
		t.Errorf("root task page has no provenance strip")
	}

	// The /work view lists every node and the rollup header.
	work := get("/work/" + root.ID)
	for _, want := range []string{"Work " + root.ID[:8], "the plan", "planned child", "verify child", "plan batch", "attempts", "cost", "tokens in/out"} {
		if !strings.Contains(work, want) {
			t.Errorf("/work/%s: missing %q", root.ID[:8], want)
		}
	}

	// Any member id canonicalizes to the root's URL.
	noRedirect := &http.Client{CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	resp, err := noRedirect.Get(srv.URL + "/work/" + verified.ID)
	if err != nil {
		t.Fatal(err)
	}
	if err := resp.Body.Close(); err != nil {
		t.Fatal(err)
	}
	if resp.StatusCode != http.StatusFound || resp.Header.Get("Location") != "/work/"+root.ID {
		t.Errorf("/work/%s = %d %q, want 302 → /work/%s", verified.ID[:8], resp.StatusCode, resp.Header.Get("Location"), root.ID[:8])
	}
}
