// bridge.go is the plugin's brain, both directions. Outbound: which journal
// entries become mail (per-kind toggles, a failure-storm debounce, and an
// edge-triggered budget-throttle check — the same rules the notify plugin
// uses), each subject carrying a `[forge #<task>]` marker. Inbound: one poll
// loop over unread mail, where a *reply* (the marker survives in "Re: …")
// answers that task's open question and anything else goes to Forge's
// concierge, which files a task or just replies.
//
// Everything here is pure with respect to the injected mailer, inbox, forgeAPI,
// and clock, so tests never open a socket.
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
	// failureDebounce is the failure-storm window: at most one failure mail per
	// window, later failures coalescing into a count in the next one.
	failureDebounce = 5 * time.Minute
	// throttleCheckEvery bounds how often journal activity may trigger a queue
	// read for the throttle edge.
	throttleCheckEvery = 30 * time.Second
	// pollEvery is the journal fallback cadence when SSE is unavailable.
	pollEvery = 3 * time.Second
	// seenCap bounds the inbound dedupe set.
	seenCap = 500
)

// mailer sends one message (SMTP, Graph, or a test recorder).
type mailer interface {
	send(ctx context.Context, m outgoing) error
}

// inbox returns the unread messages and marks them read.
type inbox interface {
	fetch(ctx context.Context) ([]inboundMail, error)
}

// journalAPI is the journal transport: the SSE follow, the polling fallback,
// and the cursor ack.
type journalAPI interface {
	Follow(ctx context.Context, since int64, emit func(journalEntry) error) error
	Since(ctx context.Context, since int64, limit int) ([]journalEntry, error)
	Ack(ctx context.Context, cursor int64) error
}

// forgeAPI is every other daemon call the bridge makes, so tests substitute a
// fake. The concrete implementation is *client.
type forgeAPI interface {
	Attention(ctx context.Context) (*attentionView, error)
	Task(ctx context.Context, id string) (*taskDetail, error)
	Queue(ctx context.Context) ([]queueItem, error)
	AnswerQuestion(ctx context.Context, questionID, answer string) error
	Assistant(ctx context.Context, sender, text string) (string, error)
}

// bridge holds both directions' shared state. The mutex guards seen and
// seenIDs, which the inbound loop and the journal loop both touch.
type bridge struct {
	api     forgeAPI
	journal journalAPI
	out     mailer
	in      inbox
	cfg     config
	log     *slog.Logger
	now     func() time.Time

	mu        sync.Mutex
	seen      map[string]bool // question ids already mailed
	seenIDs   []string        // inbound message ids already acted on, oldest first
	stateFile string

	// Outbound-loop-only state (one goroutine, no lock needed).
	lastFailureSent time.Time
	suppressed      int
	lastThrottle    time.Time
	throttled       bool
}

// mail is the one place a message leaves; a failure is logged, not fatal.
func (b *bridge) mail(ctx context.Context, m outgoing) {
	if err := b.out.send(ctx, m); err != nil && ctx.Err() == nil {
		b.log.Warn("send mail", "subject", m.Subject, "err", err)
	}
}

// ---- outbound: journal -> mail ----

// handle turns one journal entry into mail per the toggles. Every entry also
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
		b.mail(ctx, outgoing{
			Subject: "Forge: test message",
			Body:    "The email bridge is connected.\n\nReply to a question mail to answer it, or write a new mail to file a task.\n\n" + b.cfg.UI,
		})
	}
	if b.cfg.Throttling {
		b.checkThrottle(ctx)
	}
}

// onQuestion mails every open question not mailed before (deduped by id). The
// subject marker is what makes a plain reply an answer.
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
		routine := ""
		if d, terr := b.api.Task(ctx, q.WorkID); terr == nil {
			routine = d.Work.RoutineName
		}
		headline := "Forge needs input"
		if routine != "" {
			headline = "Forge: " + routine + " needs input"
		}
		body := strings.Join([]string{
			q.Text,
			"",
			"Reply to this message with your answer — the subject tag is how it finds its way back.",
			"",
			b.cfg.UI + "/tasks/" + q.WorkID,
		}, "\n")
		b.mail(ctx, outgoing{Subject: subjectLine(q.WorkID, headline), Body: body})
	}
}

// transitionPayload is the slice of a target.transition journal payload used to
// mail a failure.
type transitionPayload struct {
	Repository       string `json:"repository"`
	WorkID           string `json:"work_id"`
	To               string `json:"to"`
	Reason           string `json:"reason"`
	UnverifiedReason string `json:"unverified_reason"`
}

// onTransition mails a target that failed or went unverified, at most once per
// failureDebounce; failures inside the window are counted and reported in the
// next mail. Mail is a slower channel than a toast, so the window is wider.
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
	lines := []string{"State: " + p.To, "Reason: " + reason}
	if p.Repository != "" {
		lines = append(lines, "Repository: "+p.Repository)
	}
	if p.WorkID != "" {
		if d, err := b.api.Task(ctx, p.WorkID); err == nil && d.Work.RoutineName != "" {
			lines = append(lines, "Routine: "+d.Work.RoutineName)
		}
	}
	if b.suppressed > 0 {
		lines = append(lines, fmt.Sprintf("(%d more failures in the last %s are not mailed separately.)", b.suppressed, failureDebounce))
		b.suppressed = 0
	}
	url := b.cfg.UI
	if p.WorkID != "" {
		url = b.cfg.UI + "/tasks/" + p.WorkID
	}
	lines = append(lines, "", url)
	b.mail(ctx, outgoing{
		Subject: subjectLine(p.WorkID, "Forge: target "+p.To+" — "+reason),
		Body:    strings.Join(lines, "\n"),
	})
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
	b.mail(ctx, outgoing{
		Subject: "Forge: new proposal (" + p.Kind + ")",
		Body:    p.Kind + ": " + p.Target + "\n\n" + b.cfg.UI + "/proposals",
	})
}

// checkThrottle mails the false→true edge of a budget hard stop. Like the
// notify plugin, the signal is read off the queue (a Work deferred with reason
// hard_stop:<window>) because the store writes no dedicated journal kind.
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
		b.mail(ctx, outgoing{
			Subject: "Forge: budget hard stop (" + window + ")",
			Body:    "Work is deferred until the " + window + " window resets.\n\n" + b.cfg.UI,
		})
	}
	b.throttled = stopped
}

// markSeen records a question id and reports whether it had already been mailed.
func (b *bridge) markSeen(questionID string) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.seen[questionID] {
		return true
	}
	b.seen[questionID] = true
	return false
}

// ---- inbound: mail -> work ----

// runInbound polls the mailbox until the context ends. A poll failure is logged
// and retried on the next tick — a mail-server outage never kills the outbound
// half.
func (b *bridge) runInbound(ctx context.Context) error {
	t := time.NewTicker(time.Duration(b.cfg.PollSeconds) * time.Second)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-t.C:
			msgs, err := b.in.fetch(ctx)
			if err != nil {
				if ctx.Err() != nil {
					return ctx.Err()
				}
				// A poll that failed part way still hands back what it read,
				// and those messages are already marked read in the mailbox —
				// act on them rather than losing the commands.
				b.log.Warn("read mailbox", "err", err)
			}
			for _, m := range msgs {
				b.consume(ctx, m)
			}
		}
	}
}

// consume acts on one message once: unseen, from an allowed sender, not machine
// generated, with text left after the quoted original is stripped.
func (b *bridge) consume(ctx context.Context, m inboundMail) {
	if b.alreadyActed(m.ID) {
		return
	}
	b.recordActed(m.ID)
	if m.Auto {
		b.log.Debug("ignoring machine-generated mail", "from", m.From, "subject", m.Subject)
		return
	}
	if sameAddress(m.From, b.cfg.From) {
		return // our own mail, echoed back by the server
	}
	if !b.allowedSender(m.From) {
		b.log.Info("ignoring mail from an address outside allowed_senders", "from", m.From)
		return
	}
	text := replyText(m.Body)
	if text == "" {
		b.log.Debug("ignoring mail with no text of its own", "from", m.From, "subject", m.Subject)
		return
	}
	// A reply keeps the subject marker, and a reply always means "answer" —
	// never a new task.
	if ref := taskRefFromSubject(m.Subject); ref != "" {
		b.answer(ctx, ref, text, m)
		return
	}
	reply, err := b.api.Assistant(ctx, "email:"+m.From, text)
	if err != nil {
		b.replyTo(ctx, m, "Sorry, I hit an error reaching Forge: "+errMessage(err))
		return
	}
	if reply != "" {
		b.replyTo(ctx, m, reply)
	}
}

// allowedSender reports whether mail from this address is honored.
func (b *bridge) allowedSender(from string) bool {
	for _, s := range b.cfg.senders() {
		if sameAddress(from, s) {
			return true
		}
	}
	return false
}

// answer resolves the open question of the task the subject marker names.
func (b *bridge) answer(ctx context.Context, taskRef, answer string, m inboundMail) {
	at, err := b.api.Attention(ctx)
	if err != nil {
		b.replyTo(ctx, m, "Couldn't read the queue: "+errMessage(err))
		return
	}
	for _, q := range at.Questions {
		if !strings.HasPrefix(q.WorkID, taskRef) {
			continue
		}
		if aerr := b.api.AnswerQuestion(ctx, q.ID, answer); aerr != nil {
			b.replyTo(ctx, m, "Answer failed: "+errMessage(aerr))
			return
		}
		b.mu.Lock()
		delete(b.seen, q.ID)
		b.mu.Unlock()
		b.replyTo(ctx, m, "Answered task "+shortID(q.WorkID)+".")
		return
	}
	b.replyTo(ctx, m, "No open question for task "+taskRef+" — it may have been answered already or moved on.")
}

// replyTo answers in the thread the human wrote in.
func (b *bridge) replyTo(ctx context.Context, m inboundMail, text string) {
	b.mail(ctx, outgoing{
		Subject:   replySubject(m.Subject),
		Body:      text + "\n\n" + b.cfg.UI,
		InReplyTo: m.MessageID,
	})
}

// ---- inbound state: dedupe across restarts ----

// inboundState is what survives a restart: the ids already acted on. The
// mailbox itself is the cursor (mail is marked read once handled); this set
// only covers the window where a crash left a message read-but-unhandled or
// handled-but-unmarked.
type inboundState struct {
	SeenIDs []string `json:"seen_ids"`
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

func (b *bridge) recordActed(id string) {
	b.mu.Lock()
	b.seenIDs = append(b.seenIDs, id)
	if len(b.seenIDs) > seenCap {
		b.seenIDs = b.seenIDs[len(b.seenIDs)-seenCap:]
	}
	b.saveStateLocked()
	b.mu.Unlock()
}

// saveStateLocked writes the inbound state (caller holds b.mu).
func (b *bridge) saveStateLocked() {
	if b.stateFile == "" {
		return
	}
	data, err := json.Marshal(inboundState{SeenIDs: b.seenIDs})
	if err != nil {
		return
	}
	if werr := os.WriteFile(b.stateFile, data, 0o600); werr != nil {
		b.log.Warn("save state", "err", werr)
	}
}

// loadState restores the inbound state on start (best effort).
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
	b.seenIDs = st.SeenIDs
	b.mu.Unlock()
}
