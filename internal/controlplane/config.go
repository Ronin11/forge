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
	Retention    RetentionConfig    `toml:"retention"`
	Reflection   ReflectionConfig   `toml:"reflection"`
	Backup       BackupConfig       `toml:"backup"`

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
		Budget:       BudgetConfig{FiveHourTarget: 0.9, SevenDayTarget: 0.9, FiveHourHardStop: 0.97, SevenDayHardStop: 0.97},
		KB:           KBConfig{Path: filepath.Join(home, "kb")},
		Sandbox:      SandboxConfig{AllowHosts: []string{"api.anthropic.com", "statsig.anthropic.com", "proxy.golang.org", "sum.golang.org", "registry.npmjs.org"}},
		Integration:  IntegrationConfig{MaxStackDepth: 2, MaxRebaseAttempts: 3},
		Repositories: RepositoriesConfig{ProjectsRoot: filepath.Join(userHome, "Projects")},
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
	if _, err := os.Stat(path); err == nil {
		meta, err := toml.DecodeFile(path, &c)
		if err != nil {
			return nil, fmt.Errorf("read %s: %w", path, err)
		}
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
	if err := c.Validate(); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	return &c, nil
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
	if err := os.WriteFile(path, []byte(header+b.String()), 0o600); err != nil {
		return false, fmt.Errorf("write %s: %w", path, err)
	}
	return true, nil
}
