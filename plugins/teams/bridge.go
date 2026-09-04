// bridge.go is the plugin's brain, both directions. Outbound: which journal
// entries become channel cards (per-kind toggles, a failure-storm debounce, and
// an edge-triggered budget-throttle check, the same rules the notify plugin
// uses). Inbound: one poll loop that turns channel messages into work —
// "/answer <task-id> <text>" resolves a waiting question, "/help" lists the
// commands, and anything else goes to Forge's concierge, which decides whether
// to file a task or just reply.
//
// Everything here is pure with respect to the injected poster, channelReader,
// forgeAPI, and clock, so tests never touch Teams or a daemon.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"strings"
	"sync"
	"time"
)

const (
	// failureDebounce is the failure-storm window: at most one failure card per
	// window, later failures coalescing into a count on the next one.
	failureDebounce = 30 * time.Second
	// throttleCheckEvery bounds how often journal activity may trigger a queue
	// read for the throttle edge.
	throttleCheckEvery = 10 * time.Second
	// pollEvery is the journal fallback cadence when SSE is unavailable.
	pollEvery = 3 * time.Second
	// seenCap bounds the inbound dedupe set; Teams message ids are ordered
	// millisecond timestamps, so the oldest are the ones to drop.
	seenCap = 500
)

// poster sends one card into the channel (the webhook, or a test recorder).
type poster interface {
	post(ctx context.Context, n note) error
}

// channelReader is the inbound half: the messages since a cursor, and who is
// allowed to command Forge.
type channelReader interface {
	messages(ctx context.Context, after time.Time) ([]incoming, error)
	allowed(from string) bool
}

// journalAPI is the journal transport: the SSE follow, the polling fallback,
// and the cursor ack. Split from forgeAPI so the outbound loop can be driven by
// a fake in tests.
type journalAPI interface {
	Follow(ctx context.Context, since int64, emit func(journalEntry) error) error
	Since(ctx context.Context, since int64, limit int) ([]journalEntry, error)
	Ack(ctx context.Context, cursor int64) error
}

// forgeAPI is every daemon call the bridge makes, so tests substitute a fake.
// The concrete implementation is *client.
type forgeAPI interface {
	Attention(ctx context.Context) (*attentionView, error)
	Task(ctx context.Context, id string) (*taskDetail, error)
	Queue(ctx context.Context) ([]queueItem, error)
	AnswerQuestion(ctx context.Context, questionID, answer string) error
	Assistant(ctx context.Context, sender, text string) (string, error)
}

// bridge holds both directions' shared state. The mutex guards seen, seenIDs,
// and lastMessage — the inbound loop and the journal loop both touch them.
type bridge struct {
	api     forgeAPI
	journal journalAPI
	out     poster
	in      channelReader
	cfg     config
	log     *slog.Logger
	now     func() time.Time

	mu          sync.Mutex
	seen        map[string]bool // question ids already carded (dedup across replays)
	seenIDs     []string        // inbound message ids already acted on, oldest first
	lastMessage time.Time       // newest inbound message timestamp acted on
	stateFile   string

	// Outbound-loop-only state (one goroutine, no lock needed).
	lastFailureSent time.Time
	suppressed      int
	lastThrottle    time.Time
	throttled       bool
}

// send is the one place a card leaves; a failure is logged, not fatal.
func (b *bridge) send(ctx context.Context, n note) {
	if err := b.out.post(ctx, n); err != nil && ctx.Err() == nil {
		b.log.Warn("teams post", "err", err)
	}
}

// ---- outbound: journal -> Teams ----

// handle turns one journal entry into a card per the toggles. Every entry also
// feeds the throttle edge check (bounded by throttleCheckEvery).
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
		b.send(ctx, note{Title: "Forge: test card", Text: "The Teams bridge is connected. Reply in this channel to file a task.", URL: b.cfg.UI})
	}
	if b.cfg.Throttling {
		b.checkThrottle(ctx)
	}
}

// onQuestion cards every open question not carded before (deduped by id), with
// the short work id a human answers against.
func (b *bridge) onQuestion(ctx context.Context) {
	at, err := b.api.Attention(ctx)
	if err != nil {
		if ctx.Err() == nil {
			b.log.Warn("read attention", "err", err)
		}
		return
	}
	for _, q := range at.Questions {
		if b.markSeen(q.ID) {
			continue
		}
		short := shortID(q.WorkID)
		facts := [][2]string{{"Task", short}}
		if d, terr := b.api.Task(ctx, q.WorkID); terr == nil && d.Work.RoutineName != "" {
			facts = append(facts, [2]string{"Routine", d.Work.RoutineName})
		}
		b.send(ctx, note{
			Title: "Forge needs input",
			Text:  q.Text + "\n\nAnswer here with: /answer " + short + " <your answer>",
			URL:   b.cfg.UI + "/tasks/" + q.WorkID,
			Facts: facts,
		})
	}
}

// transitionPayload is the slice of a target.transition journal payload used to
// card a failure.
type transitionPayload struct {
	Repository       string `json:"repository"`
	WorkID           string `json:"work_id"`
	To               string `json:"to"`
	Reason           string `json:"reason"`
	UnverifiedReason string `json:"unverified_reason"`
}

// onTransition cards a target that failed or went unverified, at most once per
// failureDebounce; failures inside the window are counted and reported with the
// next card.
func (b *bridge) onTransition(ctx context.Context, e journalEntry) {
	var p transitionPayload
	if err := json.Unmarshal(e.Payload, &p); err != nil {
		b.log.Debug("skip malformed target.transition", "journal_id", e.ID, "err", err)
		return
	}
	if p.To != "failed" && p.To != "unverified" {
		return // only surface the bad transitions
	}
	now := b.now()
	if !b.lastFailureSent.IsZero() && now.Sub(b.lastFailureSent) < failureDebounce {
		b.suppressed++
		return
	}
	reason := p.Reason
	if reason == "" {
		reason = p.UnverifiedReason
	}
	if reason == "" {
		reason = p.To
	}
	facts := [][2]string{{"Reason", reason}}
	if p.Repository != "" {
		facts = append(facts, [2]string{"Repository", p.Repository})
	}
	if p.WorkID != "" {
		if d, err := b.api.Task(ctx, p.WorkID); err == nil && d.Work.RoutineName != "" {
			facts = append(facts, [2]string{"Routine", d.Work.RoutineName})
		}
	}
	text := ""
	if b.suppressed > 0 {
		text = fmt.Sprintf("%d more failures suppressed in the last %s.", b.suppressed, failureDebounce)
		b.suppressed = 0
	}
	url := b.cfg.UI
	if p.WorkID != "" {
		url = b.cfg.UI + "/tasks/" + p.WorkID
	}
	b.send(ctx, note{Title: "Forge: target " + p.To, Text: text, URL: url, Facts: facts, Urgent: true})
	b.lastFailureSent = now
}

type proposalPayload struct {
	Kind   string `json:"kind"`
	Target string `json:"target"`
}

func (b *bridge) onProposal(ctx context.Context, e journalEntry) {
	var p proposalPayload
	if err := json.Unmarshal(e.Payload, &p); err != nil || p.Kind == "" {
		b.log.Debug("skip malformed proposal.created", "journal_id", e.ID)
		return
	}
	b.send(ctx, note{
		Title: "Forge: new proposal",
		Facts: [][2]string{{"Kind", p.Kind}, {"Target", p.Target}},
		URL:   b.cfg.UI + "/proposals",
	})
}

// checkThrottle cards the false→true edge of a budget hard stop. Like the
// notify plugin, the signal is read off the queue (a Work deferred with reason
// hard_stop:<window>) because the store writes no dedicated journal kind for it.
func (b *bridge) checkThrottle(ctx context.Context) {
	now := b.now()
	if !b.lastThrottle.IsZero() && now.Sub(b.lastThrottle) < throttleCheckEvery {
		return
	}
	b.lastThrottle = now
	items, err := b.api.Queue(ctx)
	if err != nil {
		if ctx.Err() == nil {
			b.log.Debug("read queue for throttle edge", "err", err)
		}
		return
	}
	window, stopped := "", false
	for _, it := range items {
		if strings.HasPrefix(it.Reason, "hard_stop:") {
			window, stopped = strings.TrimPrefix(it.Reason, "hard_stop:"), true
			break
		}
	}
	if stopped && !b.throttled {
		b.send(ctx, note{
			Title:  "Forge: budget hard stop",
			Text:   "Work is deferred until the window resets.",
			Facts:  [][2]string{{"Window", window}},
			URL:    b.cfg.UI,
			Urgent: true,
		})
	}
	b.throttled = stopped
}

// markSeen records a question id and reports whether it had already been carded.
func (b *bridge) markSeen(questionID string) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.seen[questionID] {
		return true
	}
	b.seen[questionID] = true
	return false
}

// ---- inbound: Teams -> work ----

// runInbound polls the channel until the context ends. A poll failure is logged
// and retried on the next tick — a Graph outage never kills the outbound half.
func (b *bridge) runInbound(ctx context.Context) error {
	t := time.NewTicker(time.Duration(b.cfg.PollSeconds) * time.Second)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-t.C:
			msgs, err := b.in.messages(ctx, b.cursor())
			if err != nil {
				if ctx.Err() != nil {
					return ctx.Err()
				}
				b.log.Warn("read teams channel", "err", err)
				continue
			}
			for _, m := range msgs {
				b.consume(ctx, m)
			}
		}
	}
}

// consume acts on one message once: unseen, from an allowed sender, carrying
// the configured command prefix.
func (b *bridge) consume(ctx context.Context, m incoming) {
	if b.alreadyActed(m.ID) {
		return
	}
	b.recordActed(m.ID, m.Created)
	if !b.in.allowed(m.From) {
		b.log.Info("ignoring message from a sender outside allowed_users", "from", m.From)
		return
	}
	text, ok := stripPrefix(m.Text, b.cfg.CommandPrefix)
	if !ok {
		return
	}
	b.command(ctx, text, m.From)
}

// stripPrefix enforces the optional command prefix: with none configured every
// message counts; with one, only messages that carry it, and it is removed.
func stripPrefix(text, prefix string) (string, bool) {
	text = strings.TrimSpace(text)
	if prefix == "" {
		return text, text != ""
	}
	if !strings.HasPrefix(strings.ToLower(text), strings.ToLower(prefix)) {
		return "", false
	}
	rest := strings.TrimSpace(text[len(prefix):])
	return rest, rest != ""
}

// command interprets one inbound message: a /-command, or a request the
// concierge turns into an action.
func (b *bridge) command(ctx context.Context, text, from string) {
	fields := strings.Fields(text)
	if len(fields) == 0 {
		return
	}
	switch strings.ToLower(fields[0]) {
	case "/help", "help":
		b.send(ctx, note{
			Title: "Forge commands",
			Text: "/answer <task-id> <text> — answer a waiting question\n" +
				"/help — this list\n" +
				"anything else — a request; Forge files it as a task or answers you",
			URL: b.cfg.UI,
		})
		return
	case "/answer":
		if len(fields) < 3 {
			b.reply(ctx, "Usage: /answer <task-id> <your answer>")
			return
		}
		rest := strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(strings.TrimPrefix(text, fields[0])), fields[1]))
		b.answer(ctx, fields[1], rest)
		return
	}
	reply, err := b.api.Assistant(ctx, "teams:"+from, text)
	if err != nil {
		b.reply(ctx, "Sorry, I hit an error reaching Forge: "+errMessage(err))
		return
	}
	if reply != "" {
		b.reply(ctx, reply)
	}
}

// answer resolves the open question of the task whose id starts with taskRef.
func (b *bridge) answer(ctx context.Context, taskRef, answer string) {
	if strings.TrimSpace(answer) == "" {
		b.reply(ctx, "Give an answer: /answer <task-id> <text>")
		return
	}
	at, err := b.api.Attention(ctx)
	if err != nil {
		b.reply(ctx, "Couldn't read the queue: "+errMessage(err))
		return
	}
	for _, q := range at.Questions {
		if !strings.HasPrefix(q.WorkID, taskRef) {
			continue
		}
		if aerr := b.api.AnswerQuestion(ctx, q.ID, answer); aerr != nil {
			b.reply(ctx, "Answer failed: "+errMessage(aerr))
			return
		}
		b.mu.Lock()
		delete(b.seen, q.ID)
		b.mu.Unlock()
		b.reply(ctx, "Answered task "+shortID(q.WorkID)+".")
		return
	}
	b.reply(ctx, "No open question for task "+taskRef+".")
}

// reply posts a plain conversational card back into the channel.
func (b *bridge) reply(ctx context.Context, text string) {
	b.send(ctx, note{Title: "Forge", Text: text, URL: b.cfg.UI})
}

// ---- inbound state: dedupe across restarts ----

// inboundState is what survives a restart: the newest message acted on and the
// recent ids, so a poll that overlaps the cursor never repeats a command.
type inboundState struct {
	LastMessage time.Time `json:"last_message"`
	SeenIDs     []string  `json:"seen_ids"`
}

func (b *bridge) cursor() time.Time {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.lastMessage
}

func (b *bridge) alreadyActed(id string) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	for _, s := range b.seenIDs {
		if s == id {
			return true
		}
	}
	return false
}

// recordActed remembers a handled message and persists the cursor. The cursor
// moves only forward, so an out-of-order page cannot rewind it.
func (b *bridge) recordActed(id string, created time.Time) {
	b.mu.Lock()
	b.seenIDs = append(b.seenIDs, id)
	if len(b.seenIDs) > seenCap {
		b.seenIDs = b.seenIDs[len(b.seenIDs)-seenCap:]
	}
	if created.After(b.lastMessage) {
		b.lastMessage = created
	}
	b.saveStateLocked()
	b.mu.Unlock()
}

// saveStateLocked writes the inbound state (caller holds b.mu).
func (b *bridge) saveStateLocked() {
	if b.stateFile == "" {
		return
	}
	data, err := json.Marshal(inboundState{LastMessage: b.lastMessage, SeenIDs: b.seenIDs})
	if err != nil {
		return
	}
	if werr := os.WriteFile(b.stateFile, data, 0o600); werr != nil {
		b.log.Warn("save state", "err", werr)
	}
}

// startAt seeds the inbound cursor for a fresh install, so channel history is
// never replayed as a command backlog.
func (b *bridge) startAt(t time.Time) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.lastMessage.IsZero() {
		b.lastMessage = t
		b.saveStateLocked()
	}
}

// loadState restores the inbound state on start (best effort). A fresh install
// starts at "now", so history is never replayed as commands.
func (b *bridge) loadState() {
	if b.stateFile == "" {
		return
	}
	data, err := os.ReadFile(b.stateFile)
	if err != nil {
		return
	}
	var st inboundState
	if json.Unmarshal(data, &st) != nil {
		return
	}
	b.mu.Lock()
	b.lastMessage, b.seenIDs = st.LastMessage, st.SeenIDs
	b.mu.Unlock()
}
