// forge-signal is a bidirectional Signal bridge (DESIGN.md §22, the plugin wire
// contract in docs/PLUGINS.md). Outbound: it consumes the daemon's journal and
// sends a Signal message per new question, target failure, and proposal (toggle
// each in signal.toml). Inbound: it polls signal-cli for messages FROM the one
// configured recipient and turns them into work — a bare message files a task,
// "/answer <id> <text>" resolves a waiting question, "/status" replies with the
// queue. signal-cli must be installed and its account registered out-of-band
// (see plugin.toml); this plugin only shells out to it.
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

// pollEvery is the journal fallback cadence when SSE is unavailable.
const pollEvery = 3 * time.Second

func main() {
	log := newLogger(os.Getenv("FORGE_LOG_LEVEL"), os.Getenv("FORGE_LOG_FORMAT"))
	if err := run(log); err != nil {
		log.Error("signal plugin exited", "err", err)
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
	cfgPath := ""
	if dir := os.Getenv("FORGE_PLUGIN_DIR"); dir != "" {
		cfgPath = filepath.Join(dir, "signal.toml")
	}
	cfg, err := loadConfig(cfgPath)
	if err != nil {
		return err
	}

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	// Unconfigured: idle instead of crash-looping, so the daemon's restart=always
	// does not spin. The operator fills in account + recipient and restarts.
	if cfg.Account == "" || cfg.Recipient == "" {
		log.Warn("signal plugin not configured — set [signal] account and recipient in signal.toml, then restart", "config", cfgPath)
		<-ctx.Done()
		return nil
	}

	b := &bridge{
		api: newClient(socket, token),
		sig: signalCLI{bin: cfg.SignalCLI, account: cfg.Account, dir: stableDir(), mu: &sync.Mutex{}},
		cfg: cfg, log: log, seen: map[string]bool{}, pending: map[int64]pendingQ{},
	}
	log.Info("signal starting", "account", cfg.Account, "recipient", cfg.Recipient, "intake", cfg.Intake)

	errc := make(chan error, 2)
	go func() { errc <- b.runOutbound(ctx) }()
	if cfg.Intake {
		go func() { errc <- b.runInbound(ctx) }()
	}
	err = <-errc
	stop()
	if errors.Is(err, context.Canceled) {
		return nil
	}
	return err
}

// bridge holds the two directions' shared state.
type bridge struct {
	api  *client
	sig  signalCLI
	cfg  config
	log  *slog.Logger
	mu   sync.Mutex
	seen map[string]bool // question ids already sent (dedup across replayed events)
	// pending maps a sent question-notification timestamp to the question it
	// asked, so a Signal *reply* to that message answers it — no /answer <id>.
	pending map[int64]pendingQ
}

type pendingQ struct{ questionID, workID string }

// send is the one place a Signal message leaves; a failure is logged, not fatal.
// It returns the sent-message timestamp so a question can be matched to a reply.
func (b *bridge) send(ctx context.Context, msg string) int64 {
	ts, err := b.sig.send(ctx, b.cfg.Recipient, msg)
	if err != nil && ctx.Err() == nil {
		b.log.Warn("signal send", "err", err)
	}
	return ts
}

// ---- outbound: journal -> Signal ----

func (b *bridge) runOutbound(ctx context.Context) error {
	cursor := b.startCursor(ctx)
	emit := func(e journalEntry) error {
		if e.ID <= cursor {
			return nil
		}
		cursor = e.ID
		b.handle(ctx, e)
		if err := b.api.Ack(ctx, cursor); err != nil && ctx.Err() == nil {
			b.log.Warn("ack cursor", "cursor", cursor, "err", err)
		}
		return nil
	}
	backoff, maxBackoff := 500*time.Millisecond, 30*time.Second
	for ctx.Err() == nil {
		err := b.api.Follow(ctx, cursor, emit)
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

func (b *bridge) poll(ctx context.Context, cursor *int64, emit func(journalEntry) error) error {
	t := time.NewTicker(pollEvery)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-t.C:
			entries, err := b.api.Since(ctx, *cursor, 500)
			if err != nil {
				if ctx.Err() != nil {
					return ctx.Err()
				}
				b.log.Warn("journal poll", "err", err)
				continue
			}
			for _, e := range entries {
				if err := emit(e); err != nil {
					return err
				}
			}
		}
	}
}

// startCursor pages to the journal tail so a start/restart does not replay old
// events as fresh Signal messages.
func (b *bridge) startCursor(ctx context.Context) int64 {
	var cursor int64
	for ctx.Err() == nil {
		entries, err := b.api.Since(ctx, cursor, 1000)
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

// handle turns one journal entry into a Signal message per the toggles.
func (b *bridge) handle(ctx context.Context, e journalEntry) {
	switch e.Kind {
	case "question.asked":
		if b.cfg.Questions {
			b.onQuestion(ctx)
		}
	case "target.transition":
		if b.cfg.Failures {
			b.onTransition(ctx, e)
		}
	case "proposal.created":
		if b.cfg.Proposals {
			b.onProposal(ctx, e)
		}
	case "notify.test":
		b.send(ctx, "Forge: test message. Reply with a request to file a task.")
	}
}

// onQuestion sends any open question not sent before (deduped by id), with the
// short work id to answer against.
func (b *bridge) onQuestion(ctx context.Context) {
	at, err := b.api.Attention(ctx)
	if err != nil {
		b.log.Warn("read attention", "err", err)
		return
	}
	for _, q := range at.Questions {
		if b.seen[q.ID] {
			continue
		}
		b.seen[q.ID] = true
		short := shortID(q.WorkID)
		ts := b.send(ctx, fmt.Sprintf("Forge needs input on task %s:\n%s\n%s/tasks/%s\nReply to this message with your answer (or /answer %s <text>).", short, q.Text, b.cfg.UI, q.WorkID, short))
		if ts != 0 {
			b.mu.Lock()
			b.pending[ts] = pendingQ{questionID: q.ID, workID: q.WorkID}
			b.mu.Unlock()
		}
	}
}

// transitionPayload is the slice of a target.transition journal payload used to
// notify a failure.
type transitionPayload struct {
	Repository string `json:"repository"`
	WorkID     string `json:"work_id"`
	To         string `json:"to"`
	State      string `json:"state"`
	Reason     string `json:"reason"`
}

func (b *bridge) onTransition(ctx context.Context, e journalEntry) {
	var p transitionPayload
	if json.Unmarshal(e.Payload, &p) != nil {
		return
	}
	state := p.To
	if state == "" {
		state = p.State
	}
	if state != "failed" && state != "unverified" {
		return // only surface the bad transitions
	}
	repo := p.Repository
	if repo == "" {
		repo = shortID(e.EntityID)
	}
	msg := fmt.Sprintf("Forge: %s %s", repo, state)
	if p.Reason != "" {
		msg += " — " + p.Reason
	}
	if p.WorkID != "" {
		msg += fmt.Sprintf("\n%s/tasks/%s", b.cfg.UI, p.WorkID)
	}
	b.send(ctx, msg)
}

type proposalPayload struct {
	Kind   string `json:"kind"`
	Target string `json:"target"`
}

func (b *bridge) onProposal(ctx context.Context, e journalEntry) {
	var p proposalPayload
	if json.Unmarshal(e.Payload, &p) != nil || p.Kind == "" {
		return
	}
	b.send(ctx, fmt.Sprintf("Forge: new proposal (%s) %s\n%s/proposals/%s", p.Kind, p.Target, b.cfg.UI, e.EntityID))
}

// ---- inbound: Signal -> work ----

func (b *bridge) runInbound(ctx context.Context) error {
	timeout := time.Duration(b.cfg.PollSeconds) * time.Second
	for ctx.Err() == nil {
		msgs, err := b.sig.receive(ctx, timeout)
		if err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			b.log.Warn("signal receive", "err", err)
			if !sleepCtx(ctx, timeout) {
				return ctx.Err()
			}
			continue
		}
		for _, m := range msgs {
			if !sameNumber(m.From, b.cfg.Recipient) {
				b.log.Info("ignoring message from non-recipient", "from", m.From)
				continue
			}
			b.command(ctx, m.Text, m.QuoteID)
		}
	}
	return ctx.Err()
}

// command interprets one inbound message: a reply to a question (answers it), a
// /-command, or a bare request that becomes a task.
func (b *bridge) command(ctx context.Context, text string, quoteID int64) {
	text = strings.TrimSpace(text)
	if text == "" {
		return
	}
	// A reply to a question notification answers that question directly.
	if quoteID != 0 {
		b.mu.Lock()
		pq, ok := b.pending[quoteID]
		b.mu.Unlock()
		if ok {
			if err := b.api.AnswerQuestion(ctx, pq.questionID, text); err != nil {
				b.send(ctx, "Answer failed: "+errMessage(err))
				return
			}
			b.mu.Lock()
			delete(b.pending, quoteID)
			delete(b.seen, pq.questionID)
			b.mu.Unlock()
			b.send(ctx, "Answered task "+shortID(pq.workID)+".")
			return
		}
	}
	if !strings.HasPrefix(text, "/") {
		b.fileTask(ctx, text)
		return
	}
	fields := strings.Fields(text)
	switch strings.ToLower(fields[0]) {
	case "/help":
		b.send(ctx, "Forge commands:\n• <any text> — file a task\n• /task <text> — file a task\n• /answer <id> <text> — answer a waiting question\n• /status — queue summary")
	case "/status":
		b.status(ctx)
	case "/task":
		b.fileTask(ctx, strings.TrimSpace(strings.TrimPrefix(text, fields[0])))
	case "/answer":
		if len(fields) < 3 {
			b.send(ctx, "Usage: /answer <task-id> <your answer>")
			return
		}
		b.answer(ctx, fields[1], strings.TrimSpace(strings.TrimPrefix(strings.TrimPrefix(text, fields[0]), " "+fields[1])))
	default:
		b.send(ctx, "Unknown command. Send /help, or just text a request to file a task.")
	}
}

func (b *bridge) fileTask(ctx context.Context, prompt string) {
	if strings.TrimSpace(prompt) == "" {
		return
	}
	repos := []string(nil)
	if b.cfg.DefaultRepo != "" {
		repos = []string{b.cfg.DefaultRepo}
	}
	if len(repos) == 0 {
		b.send(ctx, "No default repo set (default_repo in signal.toml) — can't file a task without one.")
		return
	}
	id, err := b.api.CreateTask(ctx, createTaskRequest{Prompt: prompt, Repositories: repos, SubmittedBy: "signal"})
	if err != nil {
		b.send(ctx, "Couldn't file that: "+errMessage(err))
		return
	}
	b.send(ctx, fmt.Sprintf("Filed task %s on %s.\n%s/tasks/%s", shortID(id), repos[0], b.cfg.UI, id))
}

func (b *bridge) answer(ctx context.Context, taskRef, answer string) {
	if strings.TrimSpace(answer) == "" {
		b.send(ctx, "Give an answer: /answer <task-id> <text>")
		return
	}
	at, err := b.api.Attention(ctx)
	if err != nil {
		b.send(ctx, "Couldn't read the queue: "+errMessage(err))
		return
	}
	for _, q := range at.Questions {
		if strings.HasPrefix(q.WorkID, taskRef) {
			if err := b.api.AnswerQuestion(ctx, q.ID, answer); err != nil {
				b.send(ctx, "Answer failed: "+errMessage(err))
				return
			}
			delete(b.seen, q.ID)
			b.send(ctx, "Answered task "+shortID(q.WorkID)+".")
			return
		}
	}
	b.send(ctx, "No open question for task "+taskRef+".")
}

func (b *bridge) status(ctx context.Context) {
	q, err := b.api.Queue(ctx)
	if err != nil {
		b.send(ctx, "Couldn't read the queue: "+errMessage(err))
		return
	}
	running, pending, deferred := 0, 0, 0
	for _, item := range q {
		switch item.State {
		case "running":
			running++
		case "pending", "blocked":
			pending++
		case "deferred":
			deferred++
		}
	}
	b.send(ctx, fmt.Sprintf("Forge queue: %d running, %d waiting, %d deferred (%d total).\n%s", running, pending, deferred, len(q), b.cfg.UI))
}

// ---- helpers ----

// stableDir is a working directory for signal-cli that will not vanish under it
// (the plugin's own CWD can be unlinked by a reinstall); the user home, else "/".
func stableDir() string {
	if h, err := os.UserHomeDir(); err == nil && h != "" {
		return h
	}
	return "/"
}

func shortID(id string) string {
	if len(id) > 8 {
		return id[:8]
	}
	return id
}

// sameNumber compares Signal identifiers loosely: exact, or one is a suffix of
// the other (a bare number vs a +E.164 form).
func sameNumber(a, bb string) bool {
	a, bb = strings.TrimSpace(a), strings.TrimSpace(bb)
	return a != "" && (a == bb || strings.HasSuffix(a, bb) || strings.HasSuffix(bb, a))
}

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
	return slog.New(h).With("component", "plugin.signal")
}
