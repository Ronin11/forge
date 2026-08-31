package controlplane

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/BurntSushi/toml"

	"forge/internal/logging"
)

// Config is <home>/config.toml: everything the daemon reads at start. Sections
// for later milestones exist now so the file format never changes shape.
type Config struct {
	HTTP         HTTPConfig         `toml:"http"`
	Budget       BudgetConfig       `toml:"budget"`
	KB           KBConfig           `toml:"kb"`
	Log          logging.Config     `toml:"log"`
	Sandbox      SandboxConfig      `toml:"sandbox"`
	Integration  IntegrationConfig  `toml:"integration"`
	Repositories RepositoriesConfig `toml:"repositories"`
	Run          RunConfig          `toml:"run"`
	Retention    RetentionConfig    `toml:"retention"`
	Reflection   ReflectionConfig   `toml:"reflection"`
	Backup       BackupConfig       `toml:"backup"`

	// Runners, Models, and Routing (M10, DESIGN.md §21) ship as embedded
	// defaults merged under config.toml by LoadConfig; bootstrap does not write
	// them so an upgrade refreshes prices. See config_models.go.
	Runners map[string]RunnerConfig `toml:"runners"`
	Models  map[string]ModelConfig  `toml:"models"`
	Routing RoutingConfig           `toml:"routing"`

	// PluginDirs are extra roots the daemon discovers plugins from, besides the
	// built-in <home>/plugins (DESIGN.md §17). They let a user keep out-of-tree
	// plugins in their own directories (e.g. ~/.config/forge/plugins) with no
	// change to a Forge checkout. Each entry is resolved at load: ~ expands to
	// the user's home and a relative path resolves against config.toml's own
	// directory, so what the daemon discovers does not depend on its cwd.
	PluginDirs []string `toml:"plugin_dirs"`

	path string
}

// HTTPConfig is the loopback listener.
type HTTPConfig struct {
	Listen string `toml:"listen"` // default 127.0.0.1:7340; env FORGE_HTTP overrides
}

// BudgetConfig is the M3 policy's input, validated now.
type BudgetConfig struct {
	FiveHourTarget   float64          `toml:"five_hour_target"`
	SevenDayTarget   float64          `toml:"seven_day_target"`
	FiveHourHardStop float64          `toml:"five_hour_hard_stop"`
	SevenDayHardStop float64          `toml:"seven_day_hard_stop"`
	DailyUSDCap      float64          `toml:"daily_usd_cap"`
	QuietHours       QuietHoursConfig `toml:"quiet_hours"`
	// ForecastPacing gates the forecast_over_target admission rule (§10.2): when
	// true (default), normal/backlog work is deferred if the recent burn rate
	// projected to the window reset would overshoot the target, spreading work
	// across the window. When false, only current utilization gates admission —
	// a transient end-of-session spike no longer blocks new work (worst case a
	// burst overshoots and is cut at the reset, to be restarted).
	ForecastPacing bool `toml:"forecast_pacing"`
}

// QuietHoursConfig reserves headroom for interactive use.
type QuietHoursConfig struct {
	Start   string  `toml:"start"`
	End     string  `toml:"end"`
	Reserve float64 `toml:"reserve"`
}

// KBConfig is where notes live.
type KBConfig struct {
	Path string `toml:"path"`
}

// SandboxConfig is the M8 allowlist.
type SandboxConfig struct {
	AllowHosts []string `toml:"allow_hosts"`
}

// IntegrationConfig is the M9 merge-queue tuning.
type IntegrationConfig struct {
	MaxStackDepth     int `toml:"max_stack_depth"`
	MaxRebaseAttempts int `toml:"max_rebase_attempts"`
}

// RunConfig bounds the app-lifecycle supervisor (the Repos page's Start/Stop):
// the inclusive port range the daemon leases free ports from for started apps.
type RunConfig struct {
	PortMin int `toml:"port_min"`
	PortMax int `toml:"port_max"`
}

// RepositoriesConfig is where `--repo X` looks for unregistered checkouts.
type RepositoriesConfig struct {
	ProjectsRoot string `toml:"projects_root"`
}

// RetentionConfig drives forge prune.
type RetentionConfig struct {
	TranscriptDays int `toml:"transcript_days"`
	OutputDays     int `toml:"output_days"`
	ArtifactDays   int `toml:"artifact_days"`
}

// ReflectionConfig tunes the A/B auto-revert of DESIGN.md §12: after K runs on
// a proposal's new routine generation, a regression beyond Margin (relative)
// against the previous generation's last K runs restores that generation.
type ReflectionConfig struct {
	K      int     `toml:"k"`      // runs on each side before comparing; default 5
	Margin float64 `toml:"margin"` // relative regression tolerance; default 0.20
}

// BackupConfig tunes the nightly backup loop (DESIGN.md §23).
type BackupConfig struct {
	Keep int `toml:"keep"` // archives retained under <home>/backups; default 7
}

// DefaultConfig is what bootstrap writes; userHome seeds the projects root.
func DefaultConfig(home, userHome string) Config {
	return Config{
		HTTP:         HTTPConfig{Listen: "127.0.0.1:7340"},
		Budget:       BudgetConfig{FiveHourTarget: 0.9, SevenDayTarget: 0.9, FiveHourHardStop: 0.97, SevenDayHardStop: 0.97, ForecastPacing: true},
		KB:           KBConfig{Path: filepath.Join(home, "kb")},
		Sandbox:      SandboxConfig{AllowHosts: []string{"api.anthropic.com", "statsig.anthropic.com", "proxy.golang.org", "sum.golang.org", "registry.npmjs.org"}},
		Integration:  IntegrationConfig{MaxStackDepth: 2, MaxRebaseAttempts: 3},
		Repositories: RepositoriesConfig{ProjectsRoot: filepath.Join(userHome, "Projects")},
		Run:          RunConfig{PortMin: 3000, PortMax: 3099},
		Retention:    RetentionConfig{TranscriptDays: 90, OutputDays: 30, ArtifactDays: 90},
		Reflection:   ReflectionConfig{K: 5, Margin: 0.20},
		Backup:       BackupConfig{Keep: 7},
	}
}

// LoadConfig reads config.toml, applying defaults for absent keys and refusing
// unknown ones. A missing file is the defaults.
func LoadConfig(path, home, userHome string, getenv func(string) string) (*Config, error) {
	c := DefaultConfig(home, userHome)
	c.path = path
	var meta toml.MetaData
	if _, err := os.Stat(path); err == nil {
		m, err := toml.DecodeFile(path, &c)
		if err != nil {
			return nil, fmt.Errorf("read %s: %w", path, err)
		}
		meta = m
		if undecoded := meta.Undecoded(); len(undecoded) > 0 {
			keys := make([]string, len(undecoded))
			for i, k := range undecoded {
				keys[i] = k.String()
			}
			return nil, fmt.Errorf("%s: unknown keys: %s", path, strings.Join(keys, ", "))
		}
	} else if !os.IsNotExist(err) {
		return nil, fmt.Errorf("stat %s: %w", path, err)
	}
	if v := getenv("FORGE_HTTP"); v != "" {
		c.HTTP.Listen = v
	}
	base := filepath.Dir(path)
	for i, d := range c.PluginDirs {
		c.PluginDirs[i] = expandPath(d, base, userHome)
	}
	c.applyModelDefaults(meta)
	if err := c.Validate(); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	return &c, nil
}

// expandPath resolves ~ to userHome and a relative path against base, yielding
// a clean absolute path. Used for plugin_dirs so discovery is independent of
// the daemon's working directory.
func expandPath(p, base, userHome string) string {
	if p == "" {
		return p
	}
	switch {
	case p == "~":
		p = userHome
	case strings.HasPrefix(p, "~/"):
		p = filepath.Join(userHome, p[2:])
	}
	if !filepath.IsAbs(p) {
		p = filepath.Join(base, p)
	}
	return filepath.Clean(p)
}

// PluginRoots is the ordered list of directories plugin discovery scans: the
// built-in <home>/plugins first, then each configured plugin_dir. Order is the
// shadowing order — an earlier root wins a duplicate name (DESIGN.md §17), so a
// locally installed plugin takes precedence over a configured one. A configured
// root that is missing or unreadable is reported through warn and still listed
// (never fatal): one created after startup is picked up on the next restart.
func (c *Config) PluginRoots(home string, warn func(dir string, err error)) []string {
	roots := make([]string, 0, 1+len(c.PluginDirs))
	roots = append(roots, filepath.Join(home, "plugins"))
	for _, d := range c.PluginDirs {
		if warn != nil {
			if _, err := os.Stat(d); err != nil {
				warn(d, err)
			}
		}
		roots = append(roots, d)
	}
	return roots
}

// Validate is applied at load so a bad value never reaches a running daemon.
func (c *Config) Validate() error {
	if c.HTTP.Listen == "" {
		return fmt.Errorf("[http] listen is required")
	}
	b := c.Budget
	for name, v := range map[string]float64{"five_hour_target": b.FiveHourTarget, "seven_day_target": b.SevenDayTarget, "five_hour_hard_stop": b.FiveHourHardStop, "seven_day_hard_stop": b.SevenDayHardStop} {
		if v < 0 || v > 1 {
			return fmt.Errorf("[budget] %s must be in 0..1", name)
		}
	}
	if b.FiveHourHardStop < b.FiveHourTarget || b.SevenDayHardStop < b.SevenDayTarget {
		return fmt.Errorf("[budget] hard stops must not be below targets")
	}
	if (b.QuietHours.Start == "") != (b.QuietHours.End == "") {
		return fmt.Errorf("[budget.quiet_hours] start and end go together")
	}
	if b.QuietHours.Start != "" {
		for _, v := range []string{b.QuietHours.Start, b.QuietHours.End} {
			if _, err := time.Parse("15:04", v); err != nil {
				return fmt.Errorf("[budget.quiet_hours] %q: want HH:MM", v)
			}
		}
	}
	if c.Integration.MaxStackDepth < 0 || c.Integration.MaxRebaseAttempts < 1 {
		return fmt.Errorf("[integration] max_stack_depth ≥ 0 and max_rebase_attempts ≥ 1")
	}
	if c.Reflection.K < 1 {
		return fmt.Errorf("[reflection] k must be ≥ 1")
	}
	if c.Reflection.Margin <= 0 || c.Reflection.Margin >= 1 {
		return fmt.Errorf("[reflection] margin must be in (0, 1)")
	}
	if c.Backup.Keep < 1 {
		return fmt.Errorf("[backup] keep must be ≥ 1")
	}
	if err := c.validateModels(); err != nil {
		return err
	}
	return c.Log.Validate()
}

// WriteDefaultConfig writes DefaultConfig to path if absent (bootstrap).
func WriteDefaultConfig(path, home, userHome string) (written bool, err error) {
	if _, err := os.Stat(path); err == nil {
		return false, nil
	} else if !os.IsNotExist(err) {
		return false, fmt.Errorf("stat %s: %w", path, err)
	}
	var b strings.Builder
	if err := toml.NewEncoder(&b).Encode(DefaultConfig(home, userHome)); err != nil {
		return false, fmt.Errorf("encode default config: %w", err)
	}
	header := "# Forge daemon configuration. Written by bootstrap; edit freely.\n\n"
	out := header + b.String()
	// Bootstrap does not persist the embedded [runners]/[models]/[routing]
	// tables (config_models.go): keeping them out of config.toml lets a Forge
	// upgrade refresh the built-in prices and routing policy. The nil runner
	// and model maps are already omitted by the encoder; the zero [routing]
	// table — always the final emitted table, since it is the last struct
	// field — is trimmed here so the file does not ship misleading zeros.
	if i := strings.Index(out, "\n[routing]"); i >= 0 {
		out = strings.TrimRight(out[:i], "\n") + "\n"
	}
	if err := os.WriteFile(path, []byte(out), 0o600); err != nil {
		return false, fmt.Errorf("write %s: %w", path, err)
	}
	return true, nil
}
