package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func loadConfigFrom(t *testing.T, toml string) *Config {
	t.Helper()
	home := t.TempDir()
	path := filepath.Join(home, "config.toml")
	if toml != "" {
		if err := os.WriteFile(path, []byte(toml), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	c, err := LoadConfig(path, home, home, func(string) string { return "" })
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	return c
}

// The embedded defaults are present with no config file at all.
func TestEmbeddedModelDefaults(t *testing.T) {
	c := loadConfigFrom(t, "")
	if _, ok := c.Runners["claude"]; !ok {
		t.Fatalf("claude runner missing: %+v", c.Runners)
	}
	for _, alias := range []string{"haiku", "sonnet", "opus"} {
		m, ok := c.Models[alias]
		if !ok {
			t.Fatalf("model %s missing", alias)
		}
		if m.Runner != "claude" || m.ID == "" || m.MaxTier != 3 {
			t.Errorf("model %s: %+v", alias, m)
		}
	}
	if c.Models["haiku"].Class != "small" || c.Models["sonnet"].Class != "mid" || c.Models["opus"].Class != "frontier" {
		t.Errorf("classes: %s %s %s", c.Models["haiku"].Class, c.Models["sonnet"].Class, c.Models["opus"].Class)
	}
	if c.Routing.MinSamples != 5 || c.Routing.Explore != 0.1 || c.Routing.MinVerifiedSuccess != 0.6 {
		t.Errorf("routing defaults: %+v", c.Routing)
	}
	if c.Routing.Weights.USD != 1.0 {
		t.Errorf("weights: %+v", c.Routing.Weights)
	}
}

// A per-key price override keeps the rest of the model's fields.
func TestModelPriceOverride(t *testing.T) {
	c := loadConfigFrom(t, `
[models.haiku]
price = { input = 9.99 }
`)
	m := c.Models["haiku"]
	if m.Price.Input != 9.99 {
		t.Errorf("override not applied: %+v", m.Price)
	}
	if m.Price.Output != 5.00 || m.Runner != "claude" || m.Class != "small" || m.ID == "" {
		t.Errorf("override lost inherited fields: %+v", m)
	}
}

// A new runner + model is added and resolvable, and inherits nothing it did not set.
func TestNewRunnerModel(t *testing.T) {
	c := loadConfigFrom(t, `
[runners.devbox]
kind = "openai-compatible"
billing = "api"
capacity = 1
endpoint = "http://localhost:8080/v1"

[models.kimi]
runner = "devbox"
id = "kimi-k2"
class = "mid"
max_tier = 1
`)
	info, ok := c.ModelInfoFor("kimi")
	if !ok {
		t.Fatal("kimi not resolvable")
	}
	if info.Runner != "devbox" || info.RunnerKind != "openai-compatible" || info.Billing != "api" || info.MaxTier != 1 {
		t.Errorf("kimi info: %+v", info)
	}
	r := c.Runners["devbox"]
	if r.Capacity != 1 || r.Endpoint == "" {
		t.Errorf("devbox runner: %+v", r)
	}
}

func TestModelValidation(t *testing.T) {
	cases := []struct{ name, toml, want string }{
		{"unknown runner", "[models.x]\nrunner=\"nope\"\nid=\"i-1\"\nclass=\"mid\"\nmax_tier=1\n", "names no"},
		{"bad class", "[models.x]\nrunner=\"claude\"\nid=\"i-1\"\nclass=\"huge\"\nmax_tier=1\n", "class"},
		{"bad tier", "[models.x]\nrunner=\"claude\"\nid=\"i-1\"\nclass=\"mid\"\nmax_tier=9\n", "max_tier"},
		{"openai no endpoint", "[runners.d]\nkind=\"openai-compatible\"\nbilling=\"api\"\n", "endpoint"},
		{"bad kind", "[runners.d]\nkind=\"weird\"\nbilling=\"api\"\n", "kind"},
		{"negative weight", "[routing.weights]\nusd = -1\n", "must not be negative"},
		{"explore out of range", "[routing]\nexplore = 2\n", "explore"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			home := t.TempDir()
			path := filepath.Join(home, "config.toml")
			if err := os.WriteFile(path, []byte(tc.toml), 0o600); err != nil {
				t.Fatal(err)
			}
			_, err := LoadConfig(path, home, home, func(string) string { return "" })
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("want error containing %q, got %v", tc.want, err)
			}
		})
	}
}

// Bootstrap does not persist the embedded [runners]/[models]/[routing] tables.
func TestBootstrapOmitsEmbedded(t *testing.T) {
	home := t.TempDir()
	path := filepath.Join(home, "config.toml")
	if _, err := WriteDefaultConfig(path, home, home); err != nil {
		t.Fatal(err)
	}
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	for _, banned := range []string{"[routing]", "[models", "[runners"} {
		if strings.Contains(string(b), banned) {
			t.Errorf("bootstrap wrote %q:\n%s", banned, b)
		}
	}
	// And it still loads with the embedded defaults merged back in.
	c, err := LoadConfig(path, home, home, func(string) string { return "" })
	if err != nil {
		t.Fatalf("reload bootstrap config: %v", err)
	}
	if _, ok := c.ModelInfoFor("haiku"); !ok {
		t.Error("haiku not resolvable after bootstrap roundtrip")
	}
}
