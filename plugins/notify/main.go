// forge-notify is the first-party desktop-notification plugin (docs/PLUGINS.md,
// DESIGN.md §22): it consumes the daemon's journal stream and raises one
// notify-send notification per new question, target failure (debounced),
// budget throttle onset, and new proposal, each behind a per-kind toggle in
// <FORGE_PLUGIN_DIR>/notify.toml. A notify.test journal entry (the UI's
// "Send test toast" button, the notify.test RPC) always raises a toast,
// outside the toggles — it exists to verify this chain end to end.
//
// Notifications are fire-and-forget yet clickable: each carries an
// `omarchy-exec-argv` hint (a JSON ["xdg-open", <url>] the Omarchy shell runs
// on click, as safe positional args, no shell) routing to the right UI page —
// a question or failure to its task, a proposal to /proposals, a throttle to
// the dashboard. The hint is carried as notification data, so it survives a
// shell restart and needs no live sender, unlike a libnotify --action. The
// URL is also in the body as a fallback. `-a Forge` and an urgency as before.
package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/exec"
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
		log.Error("notify exiting", "err", err)
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
	path := ""
	if dir := os.Getenv("FORGE_PLUGIN_DIR"); dir != "" {
		path = filepath.Join(dir, "notify.toml")
	}
	cfg, err := loadConfig(path)
	if err != nil {
		return err
	}
	cl := newClient(socket, token)
	n := &notifier{api: cl, journal: cl, log: log, cfg: cfg, send: execNotifySend, now: time.Now, ui: defaultUI}
	log.Info("notify starting", "socket", socket, "config", fmt.Sprintf("%+v", cfg))
	return n.run(ctx)
}

// execNotifySend shells out to notify-send. It is the injected send func in
// production only; tests substitute a recorder and never raise notifications.
func execNotifySend(args ...string) error {
	cmd := exec.Command("notify-send", args...)
	cmd.Stdout, cmd.Stderr = io.Discard, io.Discard
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("notify-send: %w", err)
	}
	return nil
}

// newLogger is slog to stderr — the daemon captures a plugin's stderr into
// its log with component=plugin.notify. FORGE_LOG_LEVEL may carry the
// daemon's per-component spec; this plugin reads the bare level and defaults
// to info for anything it does not recognise.
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
	return slog.New(h).With("component", "plugin.notify")
}
