// config.go reads <FORGE_PLUGIN_DIR>/email.toml: which transport carries mail
// (SMTP+IMAP, or Microsoft 365 through Graph), the addresses Forge writes from
// and to, the per-kind outbound toggles, and the credentials. `from` and `to`
// plus a usable transport are required to do anything; everything else has a
// default, and an incomplete inbound half only disables intake.
package main

import (
	"fmt"
	"os"
	"strings"

	"github.com/BurntSushi/toml"
)

// Transports. "smtp" sends over SMTP and reads over IMAP — any provider.
// "graph" does both through Microsoft Graph, which is what a Microsoft 365
// tenant with basic auth disabled needs.
const (
	modeSMTP  = "smtp"
	modeGraph = "graph"
)

// TLS modes for the SMTP connection.
const (
	tlsSTARTTLS = "starttls" // plain connection upgraded with STARTTLS (port 587)
	tlsImplicit = "implicit" // TLS from the first byte (port 465)
	tlsNone     = "none"     // plaintext; for a local relay only
)

// config is the plugin's settings.
type config struct {
	Mode string // modeSMTP | modeGraph
	From string // the address Forge sends from
	To   string // the address notified, and the only sender whose mail is honored
	UI   string // base UI URL put in message bodies
	// AllowedSenders, when set, replaces `to` as the list of addresses whose
	// mail is honored (a shared alias notifying several people, say).
	AllowedSenders []string
	PollSeconds    int
	// Outbound toggles (all default on).
	Questions  bool
	Failures   bool
	Proposals  bool
	Throttling bool
	// Intake enables inbound mail handling; default on.
	Intake bool

	SMTP  smtpConfig
	IMAP  imapConfig
	Graph graphConfig
}

// smtpConfig is the outbound server for modeSMTP.
type smtpConfig struct {
	Host     string
	Port     int
	Username string
	Password string
	TLS      string
}

// imapConfig is the inbound mailbox for modeSMTP.
type imapConfig struct {
	Host     string
	Port     int
	Username string
	Password string
	Mailbox  string
}

// graphConfig is the Entra app registration and mailbox for modeGraph.
type graphConfig struct {
	TenantID     string
	ClientID     string
	ClientSecret string
	Mailbox      string // the mailbox Forge sends from and reads: a UPN or object id
}

func (s smtpConfig) complete() bool { return s.Host != "" }
func (i imapConfig) complete() bool { return i.Host != "" && i.Username != "" }
func (g graphConfig) complete() bool {
	return g.TenantID != "" && g.ClientID != "" && g.ClientSecret != "" && g.Mailbox != ""
}

func defaultConfig() config {
	return config{
		Mode: modeSMTP, UI: defaultUI, PollSeconds: 60,
		Questions: true, Failures: true, Proposals: true, Throttling: true, Intake: true,
		SMTP: smtpConfig{Port: 587, TLS: tlsSTARTTLS},
		IMAP: imapConfig{Port: 993, Mailbox: "INBOX"},
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
		Email struct {
			Mode           string   `toml:"mode"`
			From           string   `toml:"from"`
			To             string   `toml:"to"`
			UI             string   `toml:"ui"`
			AllowedSenders []string `toml:"allowed_senders"`
			PollSeconds    int      `toml:"poll_seconds"`
			Questions      *bool    `toml:"questions"`
			Failures       *bool    `toml:"failures"`
			Proposals      *bool    `toml:"proposals"`
			Throttling     *bool    `toml:"throttling"`
			Intake         *bool    `toml:"intake"`
			SMTP           struct {
				Host         string `toml:"host"`
				Port         int    `toml:"port"`
				Username     string `toml:"username"`
				Password     string `toml:"password"`
				PasswordFile string `toml:"password_file"`
				TLS          string `toml:"tls"`
			} `toml:"smtp"`
			IMAP struct {
				Host         string `toml:"host"`
				Port         int    `toml:"port"`
				Username     string `toml:"username"`
				Password     string `toml:"password"`
				PasswordFile string `toml:"password_file"`
				Mailbox      string `toml:"mailbox"`
			} `toml:"imap"`
			Graph struct {
				TenantID         string `toml:"tenant_id"`
				ClientID         string `toml:"client_id"`
				ClientSecret     string `toml:"client_secret"`
				ClientSecretFile string `toml:"client_secret_file"`
				Mailbox          string `toml:"mailbox"`
			} `toml:"graph"`
		} `toml:"email"`
	}
	if err := toml.Unmarshal(b, &file); err != nil {
		return cfg, fmt.Errorf("parse %s: %w", path, err)
	}
	e := file.Email
	if m := strings.ToLower(strings.TrimSpace(e.Mode)); m != "" {
		if m != modeSMTP && m != modeGraph {
			return cfg, fmt.Errorf("parse %s: mode = %q, want %q or %q", path, e.Mode, modeSMTP, modeGraph)
		}
		cfg.Mode = m
	}
	cfg.From, cfg.To = strings.TrimSpace(e.From), strings.TrimSpace(e.To)
	cfg.AllowedSenders = e.AllowedSenders
	if e.UI != "" {
		cfg.UI = e.UI
	}
	if e.PollSeconds > 0 {
		cfg.PollSeconds = e.PollSeconds
	}
	for dst, src := range map[*bool]*bool{
		&cfg.Questions: e.Questions, &cfg.Failures: e.Failures,
		&cfg.Proposals: e.Proposals, &cfg.Throttling: e.Throttling, &cfg.Intake: e.Intake,
	} {
		if src != nil {
			*dst = *src
		}
	}

	smtpPass, err := readSecret(e.SMTP.Password, e.SMTP.PasswordFile)
	if err != nil {
		return cfg, fmt.Errorf("parse %s: %w", path, err)
	}
	cfg.SMTP.Host, cfg.SMTP.Username, cfg.SMTP.Password = strings.TrimSpace(e.SMTP.Host), e.SMTP.Username, smtpPass
	if e.SMTP.Port > 0 {
		cfg.SMTP.Port = e.SMTP.Port
	}
	if m := strings.ToLower(strings.TrimSpace(e.SMTP.TLS)); m != "" {
		if m != tlsSTARTTLS && m != tlsImplicit && m != tlsNone {
			return cfg, fmt.Errorf("parse %s: smtp.tls = %q, want %q, %q or %q", path, e.SMTP.TLS, tlsSTARTTLS, tlsImplicit, tlsNone)
		}
		cfg.SMTP.TLS = m
	}

	imapPass, err := readSecret(e.IMAP.Password, e.IMAP.PasswordFile)
	if err != nil {
		return cfg, fmt.Errorf("parse %s: %w", path, err)
	}
	cfg.IMAP.Host, cfg.IMAP.Username, cfg.IMAP.Password = strings.TrimSpace(e.IMAP.Host), e.IMAP.Username, imapPass
	if e.IMAP.Port > 0 {
		cfg.IMAP.Port = e.IMAP.Port
	}
	if e.IMAP.Mailbox != "" {
		cfg.IMAP.Mailbox = e.IMAP.Mailbox
	}

	graphSecret, err := readSecret(e.Graph.ClientSecret, e.Graph.ClientSecretFile)
	if err != nil {
		return cfg, fmt.Errorf("parse %s: %w", path, err)
	}
	cfg.Graph = graphConfig{
		TenantID: strings.TrimSpace(e.Graph.TenantID), ClientID: strings.TrimSpace(e.Graph.ClientID),
		ClientSecret: graphSecret, Mailbox: strings.TrimSpace(e.Graph.Mailbox),
	}
	if cfg.Graph.Mailbox == "" && cfg.Mode == modeGraph {
		cfg.Graph.Mailbox = cfg.From // the from address is the mailbox by default
	}
	return cfg, nil
}

// senders is the set of addresses whose mail is honored: allowed_senders when
// given, else the single `to` address.
func (c config) senders() []string {
	if len(c.AllowedSenders) > 0 {
		return c.AllowedSenders
	}
	if c.To != "" {
		return []string{c.To}
	}
	return nil
}

// readSecret prefers the inline value and falls back to a file, so a password
// can live in a 0600 file the toml only names.
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
