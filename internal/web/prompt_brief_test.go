package web

import (
	"context"
	"net/http"
	"strings"
	"testing"
	"time"

	"forge/internal/core/kb"
	"forge/internal/core/store"
)

// indexBrief writes one kb note into a temp kb dir and indexes it.
func indexBrief(h *harness, title, body string, created time.Time) {
	h.t.Helper()
	dir := h.t.TempDir()
	n, err := kb.WriteNew(dir, kb.New{Title: title, Type: "note", Tags: []string{"brief"}, Body: body}, created)
	if err != nil {
		h.t.Fatal(err)
	}
	err = h.st.Write(context.Background(), func(tx *store.Tx) error { return tx.IndexKbNote(context.Background(), n) })
	if err != nil {
		h.t.Fatal(err)
	}
}

func claimForPrompt(h *harness, reqID, prompt string) string {
	h.t.Helper()
	var out workCreated
	// Disjoint declared write sets: the earlier claim of this test still
	// holds its path lease (M9), and an undeclared write set would serialise.
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: prompt, Repositories: []string{"equitizr"}, Paths: []string{"zone-" + reqID + "/**"}}, &out, http.StatusCreated)
	return h.mustClaim(reqID).SystemAppend
}

func TestRepoBriefInjectedIntoClaims(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)

	// No brief in the kb: no system append.
	if got := claimForPrompt(h, "brief-r0", "before any brief"); got != "" {
		t.Fatalf("system append without a brief = %q", got)
	}

	indexBrief(h, "brief: equitizr 2026-08-01", "OLD brief body.", time.Date(2026, 8, 1, 0, 0, 0, 0, time.UTC))
	indexBrief(h, "brief: equitizr 2026-08-30", "Monorepo; run go test ./...; hot file internal/core.", time.Date(2026, 8, 30, 0, 0, 0, 0, time.UTC))
	// A note for another repository never leaks in.
	indexBrief(h, "brief: otherrepo", "wrong repo", time.Date(2026, 8, 29, 0, 0, 0, 0, time.UTC))

	got := claimForPrompt(h, "brief-r1", "with a brief")
	if !strings.Contains(got, "REPOSITORY BRIEF for equitizr") || !strings.Contains(got, "hot file internal/core") {
		t.Errorf("system append = %q", got)
	}
	if strings.Contains(got, "OLD brief body") || strings.Contains(got, "wrong repo") {
		t.Errorf("system append picked the wrong note: %q", got)
	}
}

func TestRepoBriefCapped(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	indexBrief(h, "brief: equitizr big", strings.Repeat("x", 3*briefMaxBytes), time.Date(2026, 8, 30, 0, 0, 0, 0, time.UTC))
	got := claimForPrompt(h, "brief-r2", "capped brief")
	if len(got) == 0 || len(got) > briefMaxBytes {
		t.Errorf("system append length = %d, want (0, %d]", len(got), briefMaxBytes)
	}
}
