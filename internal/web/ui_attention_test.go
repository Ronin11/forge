package web

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

	"forge/internal/core/config"
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// TestUIAttentionCountdownAndMarker renders the Human Queue and the task page and
// asserts the two fuzzy-queue affordances: a non-critical question shows its
// countdown to auto-decision, and an auto-answered question shows the "decided by
// Forge" marker with its rationale.
func TestUIAttentionCountdownAndMarker(t *testing.T) {
	ctx := context.Background()
	clock := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	st, err := store.Open(ctx, filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{Clock: func() time.Time { return clock }})
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
		// An open non-critical question (renders a countdown on the queue).
		if _, err := tx.CreateQuestion(ctx, a, protocol.QuestionRequest{Text: "which package manager?", Criticality: "normal"}); err != nil {
			return err
		}
		// A second question, auto-answered by the sweep (renders the marker on
		// the task page).
		q, err := tx.CreateQuestion(ctx, a, protocol.QuestionRequest{Text: "which base branch?", Criticality: "low"})
		if err != nil {
			return err
		}
		_, err = tx.AutoAnswerQuestion(ctx, q.ID, "main", "auto:opus", "main is the integration branch", clock)
		return err
	}); err != nil {
		t.Fatal(err)
	}

	ui, err := NewUI(st, slog.New(slog.DiscardHandler), func() time.Time { return clock })
	if err != nil {
		t.Fatal(err)
	}
	// A 4-hour active wait; no quiet hours → the queue shows "~4h…" remaining.
	ui.SetAttention(config.AttentionConfig{WaitActiveMinutes: 240, WaitQuietMinutes: 20, Model: "opus"}, config.QuietHoursConfig{})
	srv := httptest.NewServer(ui.Handler())
	defer srv.Close()

	get := func(path string) string {
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
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("%s = %d", path, resp.StatusCode)
		}
		return string(b)
	}

	att := get("/attention")
	for _, want := range []string{"Forge decides in", "~4h", `data-f-criticality="normal"`} {
		if !strings.Contains(att, want) {
			t.Errorf("/attention lacks %q", want)
		}
	}

	task := get("/tasks/" + work.ID)
	for _, want := range []string{"decided by Forge (opus)", "main is the integration branch"} {
		if !strings.Contains(task, want) {
			t.Errorf("/tasks lacks %q", want)
		}
	}
}
