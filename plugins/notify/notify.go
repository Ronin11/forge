// notify.go is the plugin's brain: which journal entries become desktop
// notifications, with per-kind toggles, a failure-storm debounce, and
// edge-triggered budget-throttle detection. Everything here is pure with
// respect to the forgeAPI, the send func, and the clock, so tests never
// launch notify-send or a daemon.
//
// Journal kinds consumed (the real kinds the store writes):
//   - question.asked   → a new Question (text and work id via /api/v1/attention)
//   - target.transition to failed/unverified → a target failure, debounced
//   - proposal.created → a new proposal (kind and target from the payload)
//
// Throttling has no dedicated journal kind — the only budget kind the store
// writes is budget.reset, which marks a window reset, not a hard stop — so
// the throttle signal is read the way the queue exposes it: any journal
// activity triggers a bounded check of /api/v1/queue for a Work deferred
// with reason hard_stop:<window>, and the false→true edge notifies once at
// critical urgency.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"time"
)

// failureDebounce is the failure-storm window: at most one failure
// notification per window, later failures coalescing into a count reported
// with the next notification.
const failureDebounce = 30 * time.Second

// throttleCheckEvery bounds how often journal activity may trigger a queue
// read for the throttle edge.
const throttleCheckEvery = 10 * time.Second

// pollEvery is the fallback cadence against a daemon without journal SSE.
const pollEvery = 2 * time.Second

// forgeAPI is every read the notifier performs, so tests substitute a fake.
// The concrete implementation is *client.
type forgeAPI interface {
	Task(ctx context.Context, id string) (*taskDetail, error)
	Attention(ctx context.Context) (*attentionView, error)
	Queue(ctx context.Context) ([]queueItem, error)
}

// journalSource is the stream side of the client, split from forgeAPI so the
// consumer loop is explicit about what it uses.
type journalSource interface {
	Follow(ctx context.Context, since int64, emit func(journalEntry) error) error
	Since(ctx context.Context, since int64, limit int) ([]journalEntry, error)
	Ack(ctx context.Context, cursor int64) error
}

// notifier consumes the journal and raises notifications through the
// injected send func. One goroutine runs it; the state below is unshared.
type notifier struct {
	api     forgeAPI
	journal journalSource
	log     *slog.Logger
	cfg     config
	send    func(args ...string) error
	now     func() time.Time
	ui      string

	// Failure debounce: the last notification time and how many failures the
	// window swallowed since.
	lastFailureSent time.Time
	suppressed      int

	// Throttle edge state and its check rate limit.
	throttled         bool
	throttleChecked   bool
	lastThrottleCheck time.Time
}

// run consumes the journal from its current tail — old entries would raise
// notifications about the past, so history is deliberately skipped — acking
// the cursor after each processed entry (the SSE transport delivers batches
// as single frames).
func (n *notifier) run(ctx context.Context) error {
	cursor := n.startCursor(ctx)
	emit := func(e journalEntry) error {
		if e.ID <= cursor {
			return nil // replay past the last ack; already handled
		}
		cursor = e.ID
		n.handle(ctx, e)
		if err := n.journal.Ack(ctx, cursor); err != nil && ctx.Err() == nil {
			n.log.Warn("ack cursor", "cursor", cursor, "err", err)
		}
		return nil
	}
	backoff := 500 * time.Millisecond
	const maxBackoff = 30 * time.Second
	for ctx.Err() == nil {
		err := n.journal.Follow(ctx, cursor, emit)
		switch {
		case ctx.Err() != nil:
			return ctx.Err()
		case errors.Is(err, errSSEUnsupported):
			n.log.Info("journal SSE unavailable; polling", "every", pollEvery)
			return n.poll(ctx, &cursor, emit)
		case err != nil:
			n.log.Warn("journal stream failed; reconnecting", "err", err, "backoff", backoff)
		default:
			n.log.Debug("journal stream ended; reconnecting", "backoff", backoff)
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

// poll is the fallback transport: the JSON journal every pollEvery, same
// cursor contract as the stream.
func (n *notifier) poll(ctx context.Context, cursor *int64, emit func(journalEntry) error) error {
	t := time.NewTicker(pollEvery)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-t.C:
		}
		entries, err := n.journal.Since(ctx, *cursor, 500)
		if err != nil {
			if ctx.Err() == nil {
				n.log.Warn("poll journal", "err", err)
			}
			continue
		}
		for _, e := range entries {
			if err := emit(e); err != nil {
				return err
			}
		}
	}
}

// startCursor pages to the journal tail so the first notification is about
// something that happened after this process started, never a replay.
func (n *notifier) startCursor(ctx context.Context) int64 {
	var cursor int64
	for ctx.Err() == nil {
		entries, err := n.journal.Since(ctx, cursor, 1000)
		if err != nil {
			n.log.Warn("scan journal for start cursor", "cursor", cursor, "err", err)
			if !sleepCtx(ctx, 2*time.Second) {
				return cursor
			}
			continue
		}
		for _, e := range entries {
			cursor = e.ID
		}
		if len(entries) < 1000 {
			break
		}
	}
	n.log.Info("journal cursor at tail", "cursor", cursor)
	return cursor
}

// handle routes one journal entry through the toggles. Malformed payloads
// are logged and skipped, never fatal. Every entry also feeds the throttle
// edge check (bounded by throttleCheckEvery).
func (n *notifier) handle(ctx context.Context, e journalEntry) {
	switch e.Kind {
	case "question.asked":
		if n.cfg.Questions {
			n.question(ctx, e)
		}
	case "target.transition":
		if n.cfg.Failures {
			n.failure(ctx, e)
		}
	case "proposal.created":
		if n.cfg.Proposals {
			n.proposal(e)
		}
	}
	if n.cfg.Throttling {
		n.checkThrottle(ctx)
	}
}

// question notifies a new Question. The question.asked payload carries only
// attempt and target ids, so the text and work id come from the attention
// list; a failed lookup degrades to a generic notification, never silence —
// a waiting agent is exactly when the human must hear about it.
func (n *notifier) question(ctx context.Context, e journalEntry) {
	title, body := "Forge: question", "a task is waiting for an answer\n"+n.ui
	if att, err := n.api.Attention(ctx); err == nil {
		for _, q := range att.Questions {
			if q.ID != e.EntityID {
				continue
			}
			routine := ""
			if d, terr := n.api.Task(ctx, q.WorkID); terr == nil {
				routine = d.Work.RoutineName
			}
			if routine != "" {
				title = "Forge: " + routine + " asks"
			} else {
				title = "Forge: a task asks"
			}
			body = q.Text + "\n" + n.ui + "/tasks/" + q.WorkID
			break
		}
	} else if ctx.Err() == nil {
		n.log.Warn("read attention for question", "question_id", e.EntityID, "err", err)
	}
	n.notify("normal", title, body)
}

// failure notifies a target.transition to failed or unverified, at most once
// per failureDebounce; failures inside the window are counted and reported
// with the next notification ("N more failures suppressed").
func (n *notifier) failure(ctx context.Context, e journalEntry) {
	var pl struct {
		To               string `json:"to"`
		Reason           string `json:"reason"`
		UnverifiedReason string `json:"unverified_reason"`
		WorkID           string `json:"work_id"`
	}
	if err := json.Unmarshal(e.Payload, &pl); err != nil {
		n.log.Debug("skip malformed target.transition", "journal_id", e.ID, "err", err)
		return
	}
	if pl.To != "failed" && pl.To != "unverified" {
		return
	}
	now := n.now()
	if !n.lastFailureSent.IsZero() && now.Sub(n.lastFailureSent) < failureDebounce {
		n.suppressed++
		return
	}
	reason := pl.Reason
	if reason == "" {
		reason = pl.UnverifiedReason
	}
	if reason == "" {
		reason = pl.To
	}
	routine := ""
	if pl.WorkID != "" {
		if d, err := n.api.Task(ctx, pl.WorkID); err == nil {
			routine = d.Work.RoutineName
		}
	}
	title := "Forge: target " + pl.To
	if routine != "" {
		title = "Forge: " + routine + " " + pl.To
	}
	body := reason + " (" + shortID(e.EntityID) + ")"
	if n.suppressed > 0 {
		body += fmt.Sprintf("; %d more failures suppressed", n.suppressed)
		n.suppressed = 0
	}
	if pl.WorkID != "" {
		body += "\n" + n.ui + "/tasks/" + pl.WorkID
	}
	n.notify("normal", title, body)
	n.lastFailureSent = now
}

// proposal notifies a new proposal from its payload (kind and target — the
// target is the proposal's name: the routine, prompt, or doc it changes).
func (n *notifier) proposal(e journalEntry) {
	var pl struct {
		Kind   string `json:"kind"`
		Target string `json:"target"`
	}
	if err := json.Unmarshal(e.Payload, &pl); err != nil || pl.Kind == "" {
		n.log.Debug("skip malformed proposal.created", "journal_id", e.ID)
		return
	}
	n.notify("normal", "Forge: new proposal", pl.Kind+": "+pl.Target+"\n"+n.ui+"/proposals")
}

// checkThrottle reads the queue for a Work deferred with reason
// hard_stop:<window> — the one place a budget hard stop is observable
// (see the package comment) — and notifies critical on the false→true edge.
// Recovery is silent; the next hard stop notifies again.
func (n *notifier) checkThrottle(ctx context.Context) {
	now := n.now()
	if n.throttleChecked && now.Sub(n.lastThrottleCheck) < throttleCheckEvery {
		return
	}
	n.throttleChecked, n.lastThrottleCheck = true, now
	q, err := n.api.Queue(ctx)
	if err != nil {
		if ctx.Err() == nil {
			n.log.Warn("read queue for throttle check", "err", err)
		}
		return
	}
	active := false
	for _, item := range q {
		if item.State == "deferred" && strings.HasPrefix(item.Reason, "hard_stop") {
			active = true
			break
		}
	}
	if active && !n.throttled {
		n.notify("critical", "Forge: budget hard stop", "work is deferred until the window resets\n"+n.ui)
	}
	n.throttled = active
}

// notify invokes the injected send func with the fixed fire-and-forget shape
// (see the package comment in main.go): -a Forge, an urgency, title, body
// with the URL on its last line. A send failure is logged, never fatal — a
// missed notification must not kill the plugin.
func (n *notifier) notify(urgency, title, body string) {
	if err := n.send("-a", "Forge", "-u", urgency, title, body); err != nil {
		n.log.Warn("notify-send failed", "title", title, "err", err)
	}
}

// shortID is the journal entity id's first 8 characters, the short-id
// convention of the UI and CLI.
func shortID(id string) string {
	if len(id) > 8 {
		return id[:8]
	}
	return id
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
