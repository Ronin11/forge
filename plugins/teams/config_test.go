package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestConfigDefaults(t *testing.T) {
	cfg, err := loadConfig("")
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Format != formatAdaptive || cfg.PollSeconds != 30 || cfg.UI != defaultUI {
		t.Fatalf("defaults wrong: %+v", cfg)
	}
	if !cfg.Questions || !cfg.Failures || !cfg.Proposals || !cfg.Throttling || !cfg.Intake {
		t.Fatalf("toggles should default on: %+v", cfg)
	}
	if cfg.Graph.complete() {
		t.Error("an empty graph table must not count as complete")
	}
}

// A missing file is the unconfigured case: defaults, no error, the plugin idles.
func TestConfigMissingFile(t *testing.T) {
	cfg, err := loadConfig(filepath.Join(t.TempDir(), "teams.toml"))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.WebhookURL != "" {
		t.Errorf("webhook_url = %q, want empty", cfg.WebhookURL)
	}
}

func TestConfigFullAndSecretFile(t *testing.T) {
	dir := t.TempDir()
	secret := filepath.Join(dir, "secret")
	if err := os.WriteFile(secret, []byte("s3cr3t\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "teams.toml")
	body := `
[teams]
webhook_url = "https://example.logic.azure.com/workflows/abc"
format = "messagecard"
ui = "http://127.0.0.1:9999"
command_prefix = "!forge"
poll_seconds = 15
proposals = false
[teams.graph]
tenant_id = "t"
client_id = "c"
client_secret_file = "` + secret + `"
team_id = "team"
channel_id = "chan"
allowed_users = ["nate@example.com"]
`
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err := loadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Format != formatMessageCard || cfg.UI != "http://127.0.0.1:9999" || cfg.PollSeconds != 15 {
		t.Fatalf("scalars wrong: %+v", cfg)
	}
	if cfg.Proposals {
		t.Error("proposals = false was not applied")
	}
	if !cfg.Questions {
		t.Error("an absent toggle must keep its default")
	}
	if cfg.CommandPrefix != "!forge" {
		t.Errorf("command_prefix = %q", cfg.CommandPrefix)
	}
	if cfg.Graph.ClientSecret != "s3cr3t" {
		t.Errorf("secret from file = %q", cfg.Graph.ClientSecret)
	}
	if !cfg.Graph.complete() {
		t.Errorf("graph should be complete: %+v", cfg.Graph)
	}
}

func TestConfigRejectsUnknownFormat(t *testing.T) {
	path := filepath.Join(t.TempDir(), "teams.toml")
	if err := os.WriteFile(path, []byte("[teams]\nformat = \"slack\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := loadConfig(path); err == nil {
		t.Fatal("an unknown card format must fail loudly, not silently default")
	}
}
