package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestConfigDefaults(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "github-issues.toml")
	body := "[[repo]]\ngithub = \"Ronin11/forge\"\nforge = \"forge\"\n"
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err := loadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.PollSeconds != defaultPollSeconds {
		t.Errorf("poll_seconds = %d, want %d", cfg.PollSeconds, defaultPollSeconds)
	}
	if len(cfg.Repos) != 1 {
		t.Fatalf("repos = %d, want 1", len(cfg.Repos))
	}
	r := cfg.Repos[0]
	if r.Mode != "implement" || r.Class != "normal" || r.Autonomy != "auto" {
		t.Errorf("defaults wrong: %+v", r)
	}
	if r.Integrate {
		t.Errorf("integrate default should be false")
	}
	if !r.Comment {
		t.Errorf("comment default should be true")
	}
	if r.Label != "" || r.DoneLabel != "" {
		t.Errorf("label/done_label default should be empty: %+v", r)
	}
}

func TestConfigPollClamp(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "github-issues.toml")
	body := "poll_seconds = 5\n[[repo]]\ngithub = \"o/n\"\nforge = \"f\"\n"
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err := loadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.PollSeconds != minPollSeconds {
		t.Errorf("poll_seconds = %d, want clamped to %d", cfg.PollSeconds, minPollSeconds)
	}
}

func TestConfigExplicitValues(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "github-issues.toml")
	body := `poll_seconds = 120
[[repo]]
github = "Ronin11/forge"
forge = "forge"
label = "forge"
mode = "review"
class = "big"
autonomy = "manual"
integrate = true
comment = false
done_label = "forge:done"
`
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err := loadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.PollSeconds != 120 {
		t.Errorf("poll_seconds = %d", cfg.PollSeconds)
	}
	r := cfg.Repos[0]
	if r.Label != "forge" || r.Mode != "review" || r.Class != "big" || r.Autonomy != "manual" {
		t.Errorf("values wrong: %+v", r)
	}
	if !r.Integrate || r.Comment || r.DoneLabel != "forge:done" {
		t.Errorf("bool/label values wrong: %+v", r)
	}
}

func TestConfigMissingFileIdles(t *testing.T) {
	cfg, err := loadConfig(filepath.Join(t.TempDir(), "absent.toml"))
	if err != nil {
		t.Fatal(err)
	}
	if len(cfg.Repos) != 0 {
		t.Errorf("absent file should yield no repos, got %d", len(cfg.Repos))
	}
	if cfg.PollSeconds != defaultPollSeconds {
		t.Errorf("absent file poll_seconds = %d", cfg.PollSeconds)
	}
}

func TestConfigEmptyPathIdles(t *testing.T) {
	cfg, err := loadConfig("")
	if err != nil {
		t.Fatal(err)
	}
	if len(cfg.Repos) != 0 {
		t.Errorf("empty path should yield no repos")
	}
}

func TestConfigBadRepoRejected(t *testing.T) {
	dir := t.TempDir()
	cases := map[string]string{
		"missing forge":      "[[repo]]\ngithub = \"o/n\"\n",
		"bad github slug":    "[[repo]]\ngithub = \"not-a-slug\"\nforge = \"f\"\n",
		"github with spaces": "[[repo]]\ngithub = \"o / n\"\nforge = \"f\"\n",
		"empty github":       "[[repo]]\nforge = \"f\"\n",
	}
	for name, body := range cases {
		path := filepath.Join(dir, "bad.toml")
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
		if _, err := loadConfig(path); err == nil {
			t.Errorf("%s: expected error, got nil", name)
		}
	}
}

func TestConfigMalformedTOML(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "bad.toml")
	if err := os.WriteFile(path, []byte("[[repo\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := loadConfig(path); err == nil {
		t.Error("malformed TOML accepted")
	}
}
