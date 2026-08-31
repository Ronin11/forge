// forge-github-issues is the first-party GitHub-issues intake plugin
// (docs/PLUGINS.md): it polls configured repositories for open issues, creates
// a Forge task per new issue (implement mode by default) linked back to the
// issue via external_refs, then watches those tasks and comments the outcome
// on the issue when they finish. GitHub is the inbox; Forge is the worker.
//
// The plugin runs the host's `gh` CLI for all GitHub calls and never handles a
// token itself — `gh` authenticates through the XDG environment the daemon
// passes through. Forge calls go over FORGE_SOCKET with FORGE_TOKEN.
package main

import (
	"context"
	"errors"
	"log/slog"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

func main() {
	log := newLogger(os.Getenv("FORGE_LOG_LEVEL"), os.Getenv("FORGE_LOG_FORMAT"))
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := run(ctx, log); err != nil && !errors.Is(err, context.Canceled) {
		log.Error("github-issues exiting", "err", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, log *slog.Logger) error {
	socket := os.Getenv("FORGE_SOCKET")
	if socket == "" {
		return errors.New("FORGE_SOCKET is not set (the daemon sets it for supervised plugins)")
	}
	token := os.Getenv("FORGE_TOKEN")
	if token == "" {
		return errors.New("FORGE_TOKEN is not set")
	}
	configPath, statePath := "", ""
	if dir := os.Getenv("FORGE_PLUGIN_DIR"); dir != "" {
		configPath = filepath.Join(dir, "github-issues.toml")
		statePath = filepath.Join(dir, "state.json")
	}
	cfg, err := loadConfig(configPath)
	if err != nil {
		return err
	}
	p := &poller{
		api: newClient(socket, token),
		gh:  execGitHub{},
		st:  loadState(statePath, log),
		cfg: cfg,
		log: log,
		now: time.Now,
		ui:  defaultUI,
	}
	return p.run(ctx)
}

// newLogger is slog to stderr — the daemon captures a plugin's stderr into its
// log with component=plugin.github-issues. FORGE_LOG_LEVEL may carry the
// daemon's per-component spec; this plugin reads the bare level and defaults to
// info for anything it does not recognise.
func newLogger(level, format string) *slog.Logger {
	lv := slog.LevelInfo
	switch strings.ToLower(strings.TrimSpace(level)) {
	case "trace", "debug":
		lv = slog.LevelDebug
	case "warn":
		lv = slog.LevelWarn
	case "error":
		lv = slog.LevelError
	}
	opts := &slog.HandlerOptions{Level: lv}
	var h slog.Handler
	if strings.EqualFold(strings.TrimSpace(format), "json") {
		h = slog.NewJSONHandler(os.Stderr, opts)
	} else {
		h = slog.NewTextHandler(os.Stderr, opts)
	}
	return slog.New(h).With("component", "plugin.github-issues")
}
