package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
	"time"

	"forge/internal/core/model"
	"forge/internal/store"
	"forge/internal/worker"
)

// runTaskLogs prints a task's timeline — journal rows and attempt events —
// from GET /api/v1/tasks/{id}/stream: everything so far and exit, or with -f
// follow until the task is terminal, reconnecting from the last id.
func runTaskLogs(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("task logs")
	follow := fs.Bool("f", false, "follow until the task is terminal")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.stderr, "usage: forge task logs [-f] ID")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.task")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("task logs", err)
	}
	id, err := resolveTaskID(ctx, cl, fs.Arg(0))
	if err != nil {
		return c.fail("task logs", err)
	}
	hooks := streamHooks{
		onJournal: func(e store.JournalEntry) bool {
			fmt.Fprintln(c.stdout, formatJournalLine(e))
			return false
		},
		onEvent: func(e attemptStreamEvent) bool {
			fmt.Fprintln(c.stdout, formatEventLine(e))
			return false
		},
	}
	state, err := followWork(ctx, cl, id, *follow, hooks)
	if err != nil {
		return c.fail("task logs", err)
	}
	if *follow && state != "" {
		fmt.Fprintf(c.stderr, "task %s: %s\n", short(id), state)
	}
	return 0
}

// attemptStreamEvent mirrors the stream's `attempt` payload.
type attemptStreamEvent struct {
	AttemptID string `json:"attempt_id"`
	store.StoredEvent
}

// streamHooks receive each decoded stream row once; returning true stops the
// follow early.
type streamHooks struct {
	onJournal func(e store.JournalEntry) bool
	onEvent   func(e attemptStreamEvent) bool
}

// followWork consumes GET /api/v1/tasks/{id}/stream. With follow it
// reconnects from the last journal id until the end event and returns the
// Work's final state; without, it returns after one catch-up pass (?once=1).
// Rows repeated across reconnects are dropped here — journal rows by id,
// events by (attempt, source, seq) — so the hooks see each row once.
func followWork(ctx context.Context, cl *cliClient, id string, follow bool, hooks streamHooks) (model.WorkState, error) {
	var lastJournal int64
	seen := map[string]int{} // attempt/source → next event seq wanted
	for {
		path := fmt.Sprintf("/api/v1/tasks/%s/stream?since=%d", id, lastJournal)
		if !follow {
			path += "&once=1"
		}
		body, err := cl.stream(ctx, path)
		if err != nil {
			var se *worker.StatusError
			if !follow || errors.As(err, &se) {
				return "", err
			}
			// The daemon is restarting or briefly away; retry from the cursor.
			select {
			case <-ctx.Done():
				return "", ctx.Err()
			case <-time.After(time.Second):
			}
			continue
		}
		state, stopped, rerr := consumeStream(body, &lastJournal, seen, hooks)
		if cerr := body.Close(); cerr != nil && rerr == nil {
			rerr = cerr
		}
		if stopped || state != "" {
			return state, nil
		}
		if !follow {
			return "", rerr // nil on a clean end-of-catch-up
		}
		if ctx.Err() != nil {
			return "", ctx.Err()
		}
		// A clean close without an end event (a drain's retry hint) or a
		// broken stream both mean the same thing: reconnect from the cursor.
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-time.After(time.Second):
		}
	}
}

// consumeStream reads SSE events until the stream closes or a hook stops it.
// It returns the final state from an end event, if one arrived.
func consumeStream(body io.Reader, lastJournal *int64, seen map[string]int, hooks streamHooks) (model.WorkState, bool, error) {
	br := bufio.NewReader(body)
	for {
		ev, err := readSSE(br)
		if err != nil {
			if errors.Is(err, io.EOF) {
				return "", false, nil
			}
			return "", false, err
		}
		switch ev.event {
		case "journal":
			var e store.JournalEntry
			if json.Unmarshal([]byte(ev.data), &e) != nil {
				continue
			}
			if e.ID <= *lastJournal {
				continue
			}
			*lastJournal = e.ID
			if hooks.onJournal != nil && hooks.onJournal(e) {
				return "", true, nil
			}
		case "attempt":
			var e attemptStreamEvent
			if json.Unmarshal([]byte(ev.data), &e) != nil {
				continue
			}
			key := e.AttemptID + "/" + e.Source
			if next, ok := seen[key]; ok && e.Seq < next {
				continue
			}
			seen[key] = e.Seq + 1
			if hooks.onEvent != nil && hooks.onEvent(e) {
				return "", true, nil
			}
		case "end":
			var e struct {
				State model.WorkState `json:"state"`
			}
			if err := json.Unmarshal([]byte(ev.data), &e); err != nil {
				return "", false, fmt.Errorf("decode end event %q: %w", ev.data, err)
			}
			return e.State, false, nil
		}
	}
}

// formatJournalLine renders one journal row as `<ts> <kind> <message>`.
func formatJournalLine(e store.JournalEntry) string {
	msg := e.EntityType + " " + short(e.EntityID)
	var p struct {
		From   string `json:"from"`
		To     string `json:"to"`
		Reason string `json:"reason"`
	}
	if json.Unmarshal(e.Payload, &p) == nil {
		if p.To != "" {
			msg += " " + p.From + "→" + p.To
		}
		if p.Reason != "" {
			msg += " (" + p.Reason + ")"
		}
	}
	return fmt.Sprintf("%s %s %s", e.Time.Local().Format(time.RFC3339), e.Kind, msg)
}

// formatEventLine renders one attempt event as `<ts> <kind> <message>`.
func formatEventLine(e attemptStreamEvent) string {
	msg := strings.TrimRight(e.Message, "\n")
	if msg == "" {
		msg = e.Name
	}
	return fmt.Sprintf("%s %s [%s] %s", e.Time.Local().Format(time.RFC3339), e.Kind, short(e.AttemptID), msg)
}
