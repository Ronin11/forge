// forge-status-file is the first-party events plugin (docs/PLUGINS.md): it
// consumes the daemon's journal stream and maintains
// ~/.local/state/forge/status.json — atomically, on every relevant event
// (debounced) and on a 5 s heartbeat — for bars and status lines to read.
// If the daemon is down the file simply goes stale; consumers treat a ts
// older than 30 s as "daemon down".
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"
)

// heartbeatEvery is the unconditional rewrite cadence: elapsed times and
// countdowns stay fresh even when nothing happens.
const heartbeatEvery = 5 * time.Second

// pollEvery is the fallback cadence against a daemon without journal SSE.
const pollEvery = 2 * time.Second

func main() {
	log := newLogger(os.Getenv("FORGE_LOG_LEVEL"), os.Getenv("FORGE_LOG_FORMAT"))
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := run(ctx, log); err != nil && !errors.Is(err, context.Canceled) {
		log.Error("status-file exiting", "err", err)
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
	path, err := statusPath()
	if err != nil {
		return err
	}
	cl := newClient(socket, token)
	p := &plugin{api: cl, journal: cl, log: log, path: path, ui: defaultUI, now: time.Now}
	log.Info("status-file starting", "socket", socket, "path", path, "plugin_dir", os.Getenv("FORGE_PLUGIN_DIR"))
	return p.run(ctx)
}

// journalSource is the stream side of the client, split from forgeAPI so the
// consumer loop is explicit about what it uses.
type journalSource interface {
	Follow(ctx context.Context, since int64, emit func(journalEntry) error) error
	Since(ctx context.Context, since int64, limit int) ([]journalEntry, error)
	Ack(ctx context.Context, cursor int64) error
}

// plugin wires the pieces: one consumer goroutine feeding cursors through the
// debouncer, and the main loop writing the file and acking.
type plugin struct {
	api     forgeAPI
	journal journalSource
	log     *slog.Logger
	path    string
	ui      string
	now     func() time.Time

	ring failureRing

	// mu guards acked, the reconnect cursor shared between the main loop
	// (which advances it after a successful write) and the consumer.
	mu    sync.Mutex
	acked int64
}

func (p *plugin) ackedCursor() int64 {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.acked
}

func (p *plugin) setAcked(c int64) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if c > p.acked {
		p.acked = c
	}
}

func (p *plugin) run(ctx context.Context) error {
	cctx, cancel := context.WithCancel(ctx)
	defer cancel()
	dirty := make(chan int64, 256)
	writes := make(chan int64, 1)
	var wg sync.WaitGroup
	wg.Add(2)
	go func() { defer wg.Done(); p.consume(cctx, dirty) }()
	go func() { defer wg.Done(); coalesce(cctx, dirty, writes, debounceWindow) }()
	defer wg.Wait()
	defer cancel()

	hb := time.NewTicker(heartbeatEvery)
	defer hb.Stop()
	var pending int64 // highest cursor delivered for writing, not yet acked
	p.write(ctx, &pending)
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case c := <-writes:
			if c > pending {
				pending = c
			}
			p.write(ctx, &pending)
		case <-hb.C:
			p.write(ctx, &pending)
		}
	}
}

// write gathers one snapshot, writes it atomically, and acks the pending
// cursor. Failures are logged and left for the next trigger — a stale file is
// the designed degraded mode, and an unacked cursor only means replay.
func (p *plugin) write(ctx context.Context, pending *int64) {
	now := p.now()
	st, err := gather(ctx, p.api, now, p.ring.recent(now, failureWindow), p.ui)
	if err != nil {
		if ctx.Err() == nil {
			p.log.Warn("snapshot skipped", "err", err)
		}
		return
	}
	b, err := json.Marshal(st)
	if err != nil {
		p.log.Error("encode status", "err", err)
		return
	}
	if err := writeAtomic(p.path, append(b, '\n')); err != nil {
		p.log.Warn("write status file", "err", err)
		return
	}
	p.log.Debug("status written", "state", st.State, "running", len(st.Running), "queued", st.Queued, "cursor", *pending)
	if *pending > p.ackedCursor() {
		if err := p.journal.Ack(ctx, *pending); err != nil {
			if ctx.Err() == nil {
				p.log.Warn("ack cursor", "cursor", *pending, "err", err)
			}
			return
		}
		p.setAcked(*pending)
	}
}

// consume delivers journal entries: SSE first, polling on a daemon without it,
// reconnecting with exponential backoff from the last acked cursor (replayed
// entries are deduplicated by the ring and harmless to the file).
func (p *plugin) consume(ctx context.Context, dirty chan<- int64) {
	cursor := p.startCursor(ctx)
	p.setAcked(cursor)
	emit := func(e journalEntry) error {
		if e.ID > cursor {
			cursor = e.ID
		}
		if !relevant(e.Kind) {
			return nil
		}
		p.noteEntry(ctx, e)
		select {
		case dirty <- e.ID:
			return nil
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	backoff := 500 * time.Millisecond
	const maxBackoff = 30 * time.Second
	for ctx.Err() == nil {
		since := cursor
		if a := p.ackedCursor(); a > 0 && a < since {
			since = a // resume from the last ack: no gaps, duplicates tolerated
		}
		err := p.journal.Follow(ctx, since, emit)
		switch {
		case ctx.Err() != nil:
			return
		case errors.Is(err, errSSEUnsupported):
			p.log.Info("journal SSE unavailable; polling", "every", pollEvery)
			p.poll(ctx, dirty, &cursor, emit)
			return
		case err != nil:
			p.log.Warn("journal stream failed; reconnecting", "err", err, "backoff", backoff)
		default:
			p.log.Debug("journal stream ended; reconnecting", "backoff", backoff)
		}
		if !sleepCtx(ctx, backoff) {
			return
		}
		if backoff *= 2; backoff > maxBackoff {
			backoff = maxBackoff
		}
	}
}

// poll is the fallback transport: the JSON journal every pollEvery, same
// cursor contract as the stream.
func (p *plugin) poll(ctx context.Context, dirty chan<- int64, cursor *int64, emit func(journalEntry) error) {
	t := time.NewTicker(pollEvery)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		}
		entries, err := p.journal.Since(ctx, *cursor, 500)
		if err != nil {
			if ctx.Err() == nil {
				p.log.Warn("poll journal", "err", err)
			}
			continue
		}
		for _, e := range entries {
			if emit(e) != nil {
				return
			}
		}
	}
}

// startCursor finds the journal tail so the first snapshot is not preceded by
// a replay of history: page through JournalSince, keeping only failures fresh
// enough for the attention window. There is no endpoint that returns the
// plugin's persisted ack, so the tail scan doubles as the restart warm-up.
func (p *plugin) startCursor(ctx context.Context) int64 {
	var cursor int64
	for ctx.Err() == nil {
		entries, err := p.journal.Since(ctx, cursor, 1000)
		if err != nil {
			p.log.Warn("scan journal for start cursor", "cursor", cursor, "err", err)
			if !sleepCtx(ctx, 2*time.Second) {
				return cursor
			}
			continue
		}
		if len(entries) == 0 {
			break
		}
		for _, e := range entries {
			cursor = e.ID
			if relevant(e.Kind) && p.now().Sub(e.Time) <= failureWindow {
				p.noteEntry(ctx, e)
			}
		}
		// Page until the journal is exhausted: the endpoint caps the limit
		// below the 1000 asked, so "fewer than asked" is not the end (that
		// mistake stopped at the cap and rescanned only a prefix of history).
	}
	p.log.Info("journal cursor at tail", "cursor", cursor)
	return cursor
}

// noteEntry keeps the failure ring: a target.transition to failed within the
// window shows as attention and last_failure. The routine name is a best-
// effort lookup — the journal payload carries the work id, not the routine.
func (p *plugin) noteEntry(ctx context.Context, e journalEntry) {
	if e.Kind != "target.transition" {
		return
	}
	var pl struct {
		To     string `json:"to"`
		Reason string `json:"reason"`
		WorkID string `json:"work_id"`
	}
	if err := json.Unmarshal(e.Payload, &pl); err != nil || pl.To != "failed" {
		return
	}
	routine := ""
	if d, err := p.api.Task(ctx, pl.WorkID); err == nil {
		routine = d.Work.RoutineName
	}
	p.ring.add(failureRecord{JournalID: e.ID, Work: pl.WorkID, Routine: routine, Reason: pl.Reason, At: e.Time})
}

// sleepCtx waits d or until cancellation; false means cancelled.
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

// statusPath is ~/.local/state/forge/status.json, honouring XDG_STATE_HOME,
// with the directory created 0755.
func statusPath() (string, error) {
	dir := os.Getenv("XDG_STATE_HOME")
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", fmt.Errorf("resolve home for the status file: %w", err)
		}
		dir = filepath.Join(home, ".local", "state")
	}
	dir = filepath.Join(dir, "forge")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", fmt.Errorf("create state dir: %w", err)
	}
	return filepath.Join(dir, "status.json"), nil
}

// newLogger is slog to stderr — the daemon captures a plugin's stderr into
// its log with component=plugin.status-file. FORGE_LOG_LEVEL may carry the
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
	return slog.New(h).With("component", "plugin.status-file")
}
