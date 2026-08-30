package controlplane

import (
	"context"
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
		work = &store.Work{RoutineID: r.ID, RoutineName: "inventory", Generation: 1, Title: "inventory run", Trigger: model.TriggerManual, Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassInteractive, Autonomy: model.AutonomyAuto}
		targets, err := tx.CreateWork(ctx, work, []string{"equitizr"}, nil)
		if err != nil {
			return err
		}
		a, err := tx.Claim(ctx, store.ClaimParams{TargetID: targets[0].ID, WorkerID: workerID, ClaimRequestID: "r1", LeaseToken: "l", MCPToken: "m", Executor: "claude-code", Model: "claude-haiku-4-5", ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAuto})
		if err != nil {
			return err
		}
		_, err = tx.InsertEvents(ctx, a.ID, protocol.SourceWorker, []protocol.Event{{Seq: 0, Time: time.Now(), Kind: protocol.KindSpanEnd, SpanID: "fetch", Name: "fetch", DurationUS: 1200, ElapsedUS: 1300}})
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
		"/":                     {"Dashboard", "laptop", "inventory"},
		"/tasks":                {"inventory run", "equitizr", "running"},
		"/tasks/" + work.ID:     {"inventory@1", "claimed", "fetch", "1ms"},
		"/routines":             {"inventory", "list files"},
		"/system":               {"laptop", "github.com/x/equitizr", "sandbox=ready"},
		"/static/style.css":     {"nav.top"},
		"/static/app.js":        {"data-timeline"},
		"/tasks/does-not-exist": {"not found"},
		"/queue":                {"Queue", "inventory run", "data-queue"},
		"/stats?since=1d":       {"Stats", "window 1d"},
		"/attention":            {"Human queue"},
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

func truncateBody(b []byte) string {
	if len(b) > 1500 {
		return string(b[:1500]) + "…"
	}
	return string(b)
}
