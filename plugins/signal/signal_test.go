package main

import (
	"encoding/json"
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
