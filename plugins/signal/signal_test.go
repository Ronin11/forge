package main

import (
	"encoding/json"
	"log/slog"
	"path/filepath"
	"testing"
)

func TestSameNumber(t *testing.T) {
	cases := map[[2]string]bool{
		{"+15551234567", "+15551234567"}: true,
		{"15551234567", "+15551234567"}:  true, // bare vs E.164
		{"+15551234567", "5551234567"}:   true,
		{"+15559999999", "+15551234567"}: false,
		{"", "+15551234567"}:             false,
	}
	for in, want := range cases {
		if got := sameNumber(in[0], in[1]); got != want {
			t.Errorf("sameNumber(%q,%q)=%v want %v", in[0], in[1], got, want)
		}
	}
}

func TestEnvelopeParse(t *testing.T) {
	line := `{"envelope":{"source":"+15551234567","dataMessage":{"message":"fix the login bug"}}}`
	var e envelope
	if err := json.Unmarshal([]byte(line), &e); err != nil {
		t.Fatal(err)
	}
	if e.Envelope.Source != "+15551234567" || e.Envelope.DataMessage.Message != "fix the login bug" {
		t.Fatalf("parsed %+v", e)
	}
}

func TestConfigDefaultsAndToggles(t *testing.T) {
	cfg, err := loadConfig("")
	if err != nil {
		t.Fatal(err)
	}
	if cfg.SignalCLI != "signal-cli" || !cfg.Questions || !cfg.Intake || cfg.PollSeconds != 10 {
		t.Fatalf("defaults wrong: %+v", cfg)
	}
}

func TestParseSendTimestamp(t *testing.T) {
	if got := parseSendTimestamp("1788219856709\n"); got != 1788219856709 {
		t.Errorf("got %d", got)
	}
	if got := parseSendTimestamp("no number here"); got != 0 {
		t.Errorf("want 0, got %d", got)
	}
	// A small integer (not a ms timestamp) is ignored.
	if got := parseSendTimestamp("42"); got != 0 {
		t.Errorf("small int should be ignored, got %d", got)
	}
}

func TestEnvelopeQuoteParse(t *testing.T) {
	line := `{"envelope":{"source":"+15551234567","dataMessage":{"message":"main","quote":{"id":1788219856709,"author":"+1"}}}}`
	var e envelope
	if err := json.Unmarshal([]byte(line), &e); err != nil {
		t.Fatal(err)
	}
	if e.Envelope.DataMessage.Quote.ID != 1788219856709 || e.Envelope.DataMessage.Message != "main" {
		t.Fatalf("parsed %+v", e)
	}
}

func TestPendingBoundAndPersist(t *testing.T) {
	dir := t.TempDir()
	nb := func() *bridge {
		return &bridge{pending: map[int64]pendingQ{}, stateFile: filepath.Join(dir, "pending.json"), log: slog.New(slog.DiscardHandler)}
	}
	b := nb()
	for i := int64(1); i <= pendingCap+50; i++ {
		b.rememberQuestion(i, "q", "w")
	}
	if len(b.pending) != pendingCap {
		t.Fatalf("bounded size = %d, want %d", len(b.pending), pendingCap)
	}
	if _, ok := b.pending[1]; ok {
		t.Error("oldest entry should have been evicted")
	}
	// Persisted → a fresh bridge reloads it.
	b2 := nb()
	b2.loadPending()
	if len(b2.pending) != pendingCap {
		t.Fatalf("reloaded size = %d", len(b2.pending))
	}
	// takePending removes and returns.
	if _, ok := b2.takePending(pendingCap + 50); !ok {
		t.Fatal("takePending missed a known entry")
	}
	if _, ok := b2.pending[pendingCap+50]; ok {
		t.Error("takePending did not remove the entry")
	}
}
