package logging

import (
	"flag"
	"fmt"
	"log/slog"
	"path/filepath"
	"strings"
)

// Output formats for the stderr sink. The file sink is always JSON.
const (
	FormatText = "text"
	FormatJSON = "json"
)

// Environment variables children inherit; a Forge process that spawns another
// Forge process sets them from its live handler (see Environ).
const (
	EnvLevel  = "FORGE_LOG_LEVEL"
	EnvFormat = "FORGE_LOG_FORMAT"
)

// Config is the [log] section of config.toml and worker.toml. Zero values mean
// "not set" so Resolve can apply precedence; Validate runs at config load so a
// bad value is reported even when a flag or env var would have hidden it.
type Config struct {
	Level     string `toml:"level"`       // same grammar as --log-level
	Format    string `toml:"format"`      // text | json
	Dir       string `toml:"dir"`         // file sink directory; default <forge home>/logs
	MaxSizeMB int    `toml:"max_size_mb"` // rotate when the live file would exceed this; default 50
	MaxFiles  int    `toml:"max_files"`   // rotated generations kept besides the live file; default 5
}

// Validate rejects a [log] section that Resolve could not use.
func (c Config) Validate() error {
	if c.Level != "" {
		if _, err := ParseLevels(c.Level, slog.LevelInfo); err != nil {
			return fmt.Errorf("[log] level: %w", err)
		}
	}
	if c.Format != "" {
		if _, err := parseFormat(c.Format); err != nil {
			return fmt.Errorf("[log] format: %w", err)
		}
	}
	if c.MaxSizeMB < 0 || c.MaxFiles < 0 {
		return fmt.Errorf("[log] max_size_mb and max_files must not be negative")
	}
	return nil
}

// Flags are the per-subcommand logging flags. Every subcommand registers them
// with AddFlags so `forge <anything> -vv` always works and always means trace.
type Flags struct {
	Level   string
	Format  string
	Verbose bool // -v: default level at least debug
	Trace   bool // -vv: default level trace
}

// AddFlags registers --log-level, --log-format, -v, -vv on fs, so no subcommand
// can spell them differently.
func AddFlags(fs *flag.FlagSet) *Flags {
	f := &Flags{}
	fs.StringVar(&f.Level, "log-level", "", "stderr log level: trace|debug|info|warn|error, or per component (store=trace,worker=debug)")
	fs.StringVar(&f.Format, "log-format", "", "stderr log format: text|json")
	fs.BoolVar(&f.Verbose, "v", false, "shorthand for --log-level debug")
	fs.BoolVar(&f.Trace, "vv", false, "shorthand for --log-level trace")
	return f
}

// Options is what New needs after precedence has been applied.
type Options struct {
	Levels Levels
	Format string
	File   FileOptions
}

// Resolve applies flag > env > config > default; the winning level spec replaces
// the others whole (a component override on the flag does not inherit an env
// default). -vv and -v only ever make the default more verbose, so
// `--log-level trace -v` is still trace; component overrides survive both.
// forgeHome is where the file sink lives by default (<forgeHome>/logs).
func Resolve(f *Flags, getenv func(string) string, cfg Config, forgeHome string) (Options, error) {
	levelSpec := firstNonEmpty(f.Level, getenv(EnvLevel), cfg.Level, "info")
	levels, err := ParseLevels(levelSpec, slog.LevelInfo)
	if err != nil {
		return Options{}, err
	}
	if f.Trace {
		levels.Default = min(levels.Default, LevelTrace)
	} else if f.Verbose {
		levels.Default = min(levels.Default, slog.LevelDebug)
	}
	format, err := parseFormat(firstNonEmpty(f.Format, getenv(EnvFormat), cfg.Format, FormatText))
	if err != nil {
		return Options{}, err
	}
	file := FileOptions{Dir: cfg.Dir, MaxBytes: int64(cfg.MaxSizeMB) << 20, MaxFiles: cfg.MaxFiles}
	if file.Dir == "" {
		file.Dir = filepath.Join(forgeHome, "logs")
	}
	if file.MaxBytes <= 0 {
		file.MaxBytes = 50 << 20
	}
	if file.MaxFiles <= 0 {
		file.MaxFiles = 5
	}
	return Options{Levels: levels, Format: format, File: file}, nil
}

func parseFormat(s string) (string, error) {
	switch f := strings.ToLower(strings.TrimSpace(s)); f {
	case FormatText, FormatJSON:
		return f, nil
	default:
		return "", fmt.Errorf("unknown log format %q (want text|json)", s)
	}
}

// Environ returns the variables a child Forge process must be started with so it
// logs at the parent's live levels and format.
func Environ(h *Handler) []string {
	return []string{EnvLevel + "=" + h.Levels().String(), EnvFormat + "=" + h.Format()}
}

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if v = strings.TrimSpace(v); v != "" {
			return v
		}
	}
	return ""
}
