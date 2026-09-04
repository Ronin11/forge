package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestConfigDefaults(t *testing.T) {
	cfg, err := loadConfig("")
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Mode != modeSMTP || cfg.PollSeconds != 60 || cfg.UI != defaultUI {
		t.Fatalf("defaults wrong: %+v", cfg)
	}
	if cfg.SMTP.Port != 587 || cfg.SMTP.TLS != tlsSTARTTLS || cfg.IMAP.Port != 993 || cfg.IMAP.Mailbox != "INBOX" {
		t.Fatalf("transport defaults wrong: %+v %+v", cfg.SMTP, cfg.IMAP)
	}
	if !cfg.Questions || !cfg.Failures || !cfg.Proposals || !cfg.Throttling || !cfg.Intake {
		t.Fatalf("toggles should default on: %+v", cfg)
	}
}

func TestConfigSMTPWithSecretFiles(t *testing.T) {
	dir := t.TempDir()
	pw := filepath.Join(dir, "smtp-pass")
	if err := os.WriteFile(pw, []byte("hunter2\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "email.toml")
	body := `
[email]
from = "forge@example.com"
to = "nate@example.com"
poll_seconds = 30
proposals = false
[email.smtp]
host = "smtp.example.com"
port = 465
tls = "implicit"
username = "forge@example.com"
password_file = "` + pw + `"
[email.imap]
host = "imap.example.com"
username = "forge@example.com"
password = "hunter2"
mailbox = "Forge"
`
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err := loadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.SMTP.Port != 465 || cfg.SMTP.TLS != tlsImplicit || cfg.SMTP.Password != "hunter2" {
		t.Fatalf("smtp wrong: %+v", cfg.SMTP)
	}
	if cfg.IMAP.Mailbox != "Forge" || cfg.IMAP.Port != 993 {
		t.Fatalf("imap wrong: %+v", cfg.IMAP)
	}
	if cfg.Proposals || !cfg.Questions {
		t.Errorf("toggles wrong: proposals=%v questions=%v", cfg.Proposals, cfg.Questions)
	}
	if got := cfg.senders(); len(got) != 1 || got[0] != "nate@example.com" {
		t.Errorf("senders = %v, want the to address", got)
	}
	out, in, reason := transports(cfg, time.Now)
	if out == nil || in == nil {
		t.Fatalf("smtp+imap should give both transports (%q)", reason)
	}
}

// mode "graph" defaults the mailbox to the from address and needs no SMTP.
func TestConfigGraphMode(t *testing.T) {
	path := filepath.Join(t.TempDir(), "email.toml")
	body := `
[email]
mode = "graph"
from = "forge@example.com"
to = "nate@example.com"
allowed_senders = ["nate@example.com", "sam@example.com"]
[email.graph]
tenant_id = "t"
client_id = "c"
client_secret = "s"
`
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err := loadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Graph.Mailbox != "forge@example.com" || !cfg.Graph.complete() {
		t.Fatalf("graph wrong: %+v", cfg.Graph)
	}
	if len(cfg.senders()) != 2 {
		t.Errorf("allowed_senders should replace the to address: %v", cfg.senders())
	}
	out, in, reason := transports(cfg, time.Now)
	if out == nil || in == nil {
		t.Fatalf("graph should give both transports (%q)", reason)
	}
}

// Missing pieces idle the plugin with an explanation rather than crash-looping.
func TestTransportsExplainWhatIsMissing(t *testing.T) {
	cfg := defaultConfig()
	if out, _, reason := transports(cfg, time.Now); out != nil || reason == "" {
		t.Fatalf("no from/to should idle with a reason, got %v %q", out, reason)
	}
	cfg.From, cfg.To = "a@example.com", "b@example.com"
	if out, _, reason := transports(cfg, time.Now); out != nil || !strings.Contains(reason, "host") {
		t.Fatalf("no smtp host should idle naming the host, got %v %q", out, reason)
	}
	cfg.Mode = modeGraph
	if out, _, reason := transports(cfg, time.Now); out != nil || !strings.Contains(reason, "tenant_id") {
		t.Fatalf("incomplete graph should idle naming the credentials, got %v %q", out, reason)
	}
	// SMTP configured but no IMAP: outbound-only, not an error.
	cfg.Mode, cfg.SMTP.Host = modeSMTP, "smtp.example.com"
	out, in, reason := transports(cfg, time.Now)
	if out == nil || in != nil || reason != "" {
		t.Fatalf("smtp without imap should be outbound-only, got out=%v in=%v %q", out, in, reason)
	}
}

func TestConfigRejectsUnknownModeAndTLS(t *testing.T) {
	dir := t.TempDir()
	for _, body := range []string{"[email]\nmode = \"pigeon\"\n", "[email]\n[email.smtp]\ntls = \"maybe\"\n"} {
		path := filepath.Join(dir, "email.toml")
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
		if _, err := loadConfig(path); err == nil {
			t.Fatalf("want an error for %q", body)
		}
	}
}
