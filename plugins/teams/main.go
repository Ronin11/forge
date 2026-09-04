// forge-teams is the first-party Microsoft Teams bridge (docs/PLUGINS.md, the
// plugin wire contract). Outbound: it consumes the daemon's journal and posts
// one channel card per new question, target failure, budget hard stop, and
// proposal (toggle each in teams.toml) through a Teams incoming webhook.
// Inbound (optional): it polls the same channel through Microsoft Graph and
// turns messages into work — "/answer <id> <text>" resolves a waiting question,
// anything else goes to Forge's concierge.
//
// The webhook URL and, for intake, the Entra app registration are created
// out-of-band (see README.md); this plugin only speaks HTTPS to them.
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
	if err := run(log); err != nil {
		log.Error("teams plugin exited", "err", err)
		os.Exit(1)
	}
}

func run(log *slog.Logger) error {
	socket := os.Getenv("FORGE_SOCKET")
	if socket == "" {
		return errors.New("FORGE_SOCKET is not set (the daemon sets it for supervised plugins)")
	}
	token := os.Getenv("FORGE_TOKEN")
	if token == "" {
		return errors.New("FORGE_TOKEN is not set")
	}
	dir := os.Getenv("FORGE_PLUGIN_DIR")
	cfgPath := ""
	if dir != "" {
		cfgPath = filepath.Join(dir, "teams.toml")
	}
	cfg, err := loadConfig(cfgPath)
	if err != nil {
		return err
	}

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	// Unconfigured: idle instead of crash-looping, so the daemon's restart=always
	// does not spin. The operator fills in the webhook URL and restarts.
	if cfg.WebhookURL == "" {
		log.Warn("teams plugin not configured — set [teams] webhook_url in teams.toml, then restart", "config", cfgPath)
		<-ctx.Done()
		return nil
	}

	api := newClient(socket, token)
	b := &bridge{
		api: api, journal: api,
		out: newWebhook(cfg.WebhookURL, cfg.Format),
		cfg: cfg, log: log, now: time.Now, seen: map[string]bool{},
	}
	intake := cfg.Intake && cfg.Graph.complete()
	if cfg.Intake && !cfg.Graph.complete() {
		log.Warn("intake disabled — [teams.graph] needs tenant_id, client_id, client_secret, team_id and channel_id")
	}
	if intake {
		b.in = newGraphClient(cfg.Graph, time.Now)
		if dir != "" {
			b.stateFile = filepath.Join(dir, "state.json")
			b.loadState()
		}
		// A fresh install starts at "now": channel history is not a command
		// backlog to replay.
		b.startAt(time.Now())
	}
	log.Info("teams starting", "format", cfg.Format, "intake", intake, "poll_seconds", cfg.PollSeconds)

	errc := make(chan error, 2)
	go func() { errc <- b.runOutbound(ctx) }()
	if intake {
		go func() { errc <- b.runInbound(ctx) }()
	}
	err = <-errc
	stop()
	if errors.Is(err, context.Canceled) {
		return nil
	}
	return err
}

// runOutbound follows the journal (SSE, falling back to polling), handing each
// new entry to the bridge and acking the cursor so a restart resumes cleanly.
func (b *bridge) runOutbound(ctx context.Context) error {
	cursor := b.startCursor(ctx)
	emit := func(e journalEntry) error {
		if e.ID <= cursor {
			return nil
		}
		cursor = e.ID
		b.handle(ctx, e)
		if err := b.journal.Ack(ctx, cursor); err != nil && ctx.Err() == nil {
			b.log.Warn("ack cursor", "cursor", cursor, "err", err)
		}
		return nil
	}
	backoff, maxBackoff := 500*time.Millisecond, 30*time.Second
	for ctx.Err() == nil {
		err := b.journal.Follow(ctx, cursor, emit)
		switch {
		case ctx.Err() != nil:
			return ctx.Err()
		case errors.Is(err, errSSEUnsupported):
			return b.poll(ctx, &cursor, emit)
		case err != nil:
			b.log.Warn("journal stream failed; reconnecting", "err", err)
		}
		if !sleepCtx(ctx, backoff) {
			return ctx.Err()
		}
		if backoff *= 2; backoff > maxBackoff {
			backoff = maxBackoff
		}
	}
	return ctx.Err()
}

// poll is the fallback transport against a daemon without journal SSE.
func (b *bridge) poll(ctx context.Context, cursor *int64, emit func(journalEntry) error) error {
	t := time.NewTicker(pollEvery)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-t.C:
			entries, err := b.journal.Since(ctx, *cursor, 500)
			if err != nil {
				if ctx.Err() != nil {
					return ctx.Err()
				}
				b.log.Warn("journal poll", "err", err)
				continue
			}
			for _, e := range entries {
				if eerr := emit(e); eerr != nil {
					return eerr
				}
			}
		}
	}
}

// startCursor pages to the journal tail so a start/restart does not replay old
// events as fresh cards.
func (b *bridge) startCursor(ctx context.Context) int64 {
	var cursor int64
	for ctx.Err() == nil {
		entries, err := b.journal.Since(ctx, cursor, 1000)
		if err != nil {
			if !sleepCtx(ctx, 2*time.Second) {
				return cursor
			}
			continue
		}
		if len(entries) == 0 {
			break
		}
		last := entries[len(entries)-1].ID
		if last <= cursor {
			break
		}
		cursor = last
	}
	b.log.Info("journal cursor at tail", "cursor", cursor)
	return cursor
}

func shortID(id string) string {
	if len(id) > 8 {
		return id[:8]
	}
	return id
}

// errMessage unwraps a daemon error to the message a human should read.
func errMessage(err error) string {
	var ae *apiError
	if errors.As(err, &ae) {
		return strings.TrimSpace(ae.Body)
	}
	return err.Error()
}

func sleepCtx(ctx context.Context, d time.Duration) bool {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-t.C:
		return true
	}
}

// newLogger is slog to stderr — the daemon captures a plugin's stderr into its
// log with component=plugin.teams.
func newLogger(level, format string) *slog.Logger {
	lv := slog.LevelInfo
	if err := lv.UnmarshalText([]byte(strings.TrimSpace(level))); err != nil {
		lv = slog.LevelInfo
	}
	opts := &slog.HandlerOptions{Level: lv}
	var h slog.Handler = slog.NewTextHandler(os.Stderr, opts)
	if strings.EqualFold(strings.TrimSpace(format), "json") {
		h = slog.NewJSONHandler(os.Stderr, opts)
	}
	return slog.New(h).With("component", "plugin.teams")
}
