// config.go reads <FORGE_PLUGIN_DIR>/teams.toml: the outbound incoming-webhook
// URL and card format, the per-kind outbound toggles, and the optional
// Microsoft Graph credentials that make intake possible. Only webhook_url is
// required to do anything; the whole [teams.graph] table is optional and, when
// incomplete, intake stays off rather than failing the plugin.
package main

import (
	"fmt"
	"os"
	"strings"

	"github.com/BurntSushi/toml"
)

// Card formats. "adaptive" is the payload a Power Automate "post to a channel
// when a webhook request is received" workflow expects; "messagecard" is the
// legacy Office 365 connector shape, kept for webhooks created before the
// connector retirement.
const (
	formatAdaptive    = "adaptive"
	formatMessageCard = "messagecard"
)

// config is the plugin's settings.
type config struct {
	WebhookURL string // the Teams incoming webhook Forge posts to
	Format     string // formatAdaptive | formatMessageCard
	UI         string // base UI URL put in card links
	// Outbound toggles (all default on).
	Questions  bool
	Failures   bool
	Proposals  bool
	Throttling bool
	// Intake enables inbound commands; it needs a complete graph table.
	Intake bool
	Graph  graphConfig
	// CommandPrefix, when set, is required on an inbound message and stripped
	// before the message is interpreted — the way to share a busy channel.
	CommandPrefix string
	PollSeconds   int
}

// graphConfig is the Entra app registration that reads channel messages. All
// five fields are needed for intake; the secret may come from a file so it
// never sits in a world-readable toml.
type graphConfig struct {
	TenantID     string
	ClientID     string
	ClientSecret string
	TeamID       string
	ChannelID    string
	// AllowedUsers restricts whose messages are honored (UPN, display name, or
	// Entra object id, matched case-insensitively). Empty means anyone who can
	// post in the channel.
	AllowedUsers []string
}

// complete reports whether intake can run: every credential and both ids.
func (g graphConfig) complete() bool {
	return g.TenantID != "" && g.ClientID != "" && g.ClientSecret != "" && g.TeamID != "" && g.ChannelID != ""
}

func defaultConfig() config {
	return config{
		Format: formatAdaptive, UI: defaultUI, PollSeconds: 30,
		Questions: true, Failures: true, Proposals: true, Throttling: true, Intake: true,
	}
}

// loadConfig reads path, tolerating its absence. Pointer fields distinguish an
// absent key (keep the default) from an explicit false.
func loadConfig(path string) (config, error) {
	cfg := defaultConfig()
	if path == "" {
		return cfg, nil
	}
	b, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return cfg, nil
	}
	if err != nil {
		return cfg, fmt.Errorf("read %s: %w", path, err)
	}
	var file struct {
		Teams struct {
			WebhookURL    string `toml:"webhook_url"`
			Format        string `toml:"format"`
			UI            string `toml:"ui"`
			CommandPrefix string `toml:"command_prefix"`
			PollSeconds   int    `toml:"poll_seconds"`
			Questions     *bool  `toml:"questions"`
			Failures      *bool  `toml:"failures"`
			Proposals     *bool  `toml:"proposals"`
			Throttling    *bool  `toml:"throttling"`
			Intake        *bool  `toml:"intake"`
			Graph         struct {
				TenantID         string   `toml:"tenant_id"`
				ClientID         string   `toml:"client_id"`
				ClientSecret     string   `toml:"client_secret"`
				ClientSecretFile string   `toml:"client_secret_file"`
				TeamID           string   `toml:"team_id"`
				ChannelID        string   `toml:"channel_id"`
				AllowedUsers     []string `toml:"allowed_users"`
			} `toml:"graph"`
		} `toml:"teams"`
	}
	if err := toml.Unmarshal(b, &file); err != nil {
		return cfg, fmt.Errorf("parse %s: %w", path, err)
	}
	t := file.Teams
	cfg.WebhookURL, cfg.CommandPrefix = strings.TrimSpace(t.WebhookURL), strings.TrimSpace(t.CommandPrefix)
	if f := strings.ToLower(strings.TrimSpace(t.Format)); f != "" {
		if f != formatAdaptive && f != formatMessageCard {
			return cfg, fmt.Errorf("parse %s: format = %q, want %q or %q", path, t.Format, formatAdaptive, formatMessageCard)
		}
		cfg.Format = f
	}
	if t.UI != "" {
		cfg.UI = t.UI
	}
	if t.PollSeconds > 0 {
		cfg.PollSeconds = t.PollSeconds
	}
	for dst, src := range map[*bool]*bool{
		&cfg.Questions: t.Questions, &cfg.Failures: t.Failures,
		&cfg.Proposals: t.Proposals, &cfg.Throttling: t.Throttling, &cfg.Intake: t.Intake,
	} {
		if src != nil {
			*dst = *src
		}
	}
	g := t.Graph
	secret, err := readSecret(g.ClientSecret, g.ClientSecretFile)
	if err != nil {
		return cfg, fmt.Errorf("parse %s: %w", path, err)
	}
	cfg.Graph = graphConfig{
		TenantID: strings.TrimSpace(g.TenantID), ClientID: strings.TrimSpace(g.ClientID),
		ClientSecret: secret, TeamID: strings.TrimSpace(g.TeamID), ChannelID: strings.TrimSpace(g.ChannelID),
		AllowedUsers: g.AllowedUsers,
	}
	return cfg, nil
}

// readSecret prefers the inline value and falls back to a file, so a secret can
// live in a 0600 file the toml only names.
func readSecret(inline, file string) (string, error) {
	if s := strings.TrimSpace(inline); s != "" {
		return s, nil
	}
	if file == "" {
		return "", nil
	}
	b, err := os.ReadFile(expandHome(file))
	if err != nil {
		return "", fmt.Errorf("read secret file %s: %w", file, err)
	}
	return strings.TrimSpace(string(b)), nil
}

// expandHome resolves a leading ~ so a config may name a path under the home
// directory, matching how config.toml's plugin_dirs are written.
func expandHome(path string) string {
	if !strings.HasPrefix(path, "~/") {
		return path
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return path
	}
	return home + path[1:]
}
