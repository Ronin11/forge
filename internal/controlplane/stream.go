package controlplane

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"sort"
	"strconv"
	"time"

	"forge/internal/model"
	"forge/internal/store"
)

// streamBatch bounds one cursor read of either feed; a tick loops until both
// feeds are drained, so the bound sizes memory, not the stream.
const streamBatch = 500

// streamRoutes registers the SSE endpoint (DESIGN.md §13): GET
// /api/v1/work/{id}/stream and its /tasks alias, which back `forge task logs
// -f` and `--wait`.
func (s *Server) streamRoutes(m *http.ServeMux) {
	for _, base := range []string{"/api/v1/work", "/api/v1/tasks"} {
		m.HandleFunc("GET "+base+"/{id}/stream", s.streamWork)
	}
}

// streamAttemptEvent is an attempt event on the wire, the attempt named so a
// client following a multi-target Work can tell the timelines apart.
type streamAttemptEvent struct {
	AttemptID string `json:"attempt_id"`
	store.StoredEvent
}

// streamEndEvent closes a finished stream with the Work's final state.
type streamEndEvent struct {
	State model.WorkState `json:"state"`
}

// streamCursor is one connection's position: the journal id high-water mark
// (the client's resume token) and each attempt's per-source event floors.
// Event floors are connection-local, so a reconnect resends events; clients
// drop repeats by (attempt, source, seq).
type streamCursor struct {
	journal int64
	events  map[string]*store.EventSeqs // attempt id → floors
}

// streamWork is GET /api/v1/work/{id}/stream?since=<journal id> (DESIGN.md
// §13): SSE carrying the Work's journal rows (`event: journal`, id = journal
// id) and its attempts' events (`event: attempt`), polled from the store every
// streamInterval. The stream ends with `event: end` once the Work is terminal
// and everything pending was sent; ?once=1 ends it at the first catch-up
// instead; a drain or shutdown closes it with a `retry:` hint so clients
// reconnect from their cursor (§1.4). It bypasses handle: it owns its writer,
// and it must not count as in-flight or a drain could never reach idle.
func (s *Server) streamWork(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	id, err := pathID(r)
	if err != nil {
		s.fail(ctx, w, err)
		return
	}
	cur := streamCursor{events: map[string]*store.EventSeqs{}}
	if raw := r.URL.Query().Get("since"); raw != "" {
		n, perr := strconv.ParseInt(raw, 10, 64)
		if perr != nil || n < 0 {
			s.fail(ctx, w, badRequest("since %q: want a journal id", raw))
			return
		}
		cur.journal = n
	}
	once := r.URL.Query().Get("once") == "1"
	work, err := s.store.GetWork(ctx, id)
	if err != nil {
		s.fail(ctx, w, err)
		return
	}
	rc := http.NewResponseController(w)
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.WriteHeader(http.StatusOK)
	if err := rc.Flush(); err != nil {
		s.log.WarnContext(ctx, "stream: response writer cannot flush", "error", err)
		return
	}
	s.log.DebugContext(ctx, "stream opened", "work_id", id, "since", cur.journal, "once", once)
	for {
		targets, err := s.store.TargetsForWork(ctx, id)
		if err != nil {
			s.log.WarnContext(ctx, "stream: read targets", "work_id", id, "error", err)
			return
		}
		// Terminal is decided before the fetch: journal rows land in the same
		// transaction as the state change, so a terminal state observed here
		// means the fetch below already sees every pending row.
		terminal := len(targets) > 0
		states := make([]model.State, len(targets))
		for i, t := range targets {
			states[i] = t.State
			if !model.IsTerminal(t.State, work.Integrate) {
				terminal = false
			}
		}
		n, err := s.streamTick(ctx, w, id, targets, &cur)
		if err != nil {
			s.log.DebugContext(ctx, "stream ended", "work_id", id, "error", err)
			return
		}
		if n > 0 {
			if err := rc.Flush(); err != nil {
				return
			}
			continue // drain the backlog before sleeping or ending
		}
		if terminal {
			state := model.DeriveWorkState(model.WorkInputs{Targets: states, Integrate: work.Integrate})
			if err := writeSSE(w, "end", cur.journal, streamEndEvent{State: state}); err != nil {
				s.log.DebugContext(ctx, "stream end write", "work_id", id, "error", err)
			}
			if err := rc.Flush(); err != nil {
				s.log.DebugContext(ctx, "stream end flush", "work_id", id, "error", err)
			}
			return
		}
		if once {
			return
		}
		if s.Draining() {
			s.streamRetryHint(ctx, w, rc)
			return
		}
		select {
		case <-ctx.Done():
			return
		case <-s.closed:
			s.streamRetryHint(ctx, w, rc)
			return
		case <-time.After(s.streamInterval):
		}
	}
}

// streamTick sends everything pending once — journal rows and attempt events
// merged oldest-first by timestamp — and advances the cursor. Journal rows
// carry their own id; an attempt event carries the id of the journal row last
// emitted before it, so ids never decrease and resuming from any delivered id
// never loses a row (it may repeat rows the client saw, which clients drop by
// id and by attempt/source/seq).
func (s *Server) streamTick(ctx context.Context, w io.Writer, workID string, targets []store.Target, cur *streamCursor) (int, error) {
	floor := cur.journal
	entries, err := s.store.JournalForWorkSince(ctx, workID, cur.journal, streamBatch)
	if err != nil {
		return 0, fmt.Errorf("read journal: %w", err)
	}
	ids := make([]string, len(targets))
	for i, t := range targets {
		ids[i] = t.ID
	}
	byTarget, err := s.store.AttemptsForTargets(ctx, ids)
	if err != nil {
		return 0, fmt.Errorf("read attempts: %w", err)
	}
	type item struct {
		ts    time.Time
		event string
		id    int64
		data  any
	}
	var items []item
	for _, e := range entries {
		items = append(items, item{ts: e.Time, event: "journal", id: e.ID, data: e})
		if e.ID > cur.journal {
			cur.journal = e.ID
		}
	}
	for _, tid := range ids {
		for _, a := range byTarget[tid] {
			seqs := cur.events[a.ID]
			if seqs == nil {
				seqs = &store.EventSeqs{}
				cur.events[a.ID] = seqs
			}
			evs, err := s.store.EventsSinceSeq(ctx, a.ID, *seqs, streamBatch)
			if err != nil {
				return 0, fmt.Errorf("read events: %w", err)
			}
			for _, ev := range evs {
				items = append(items, item{ts: ev.Time, event: "attempt", data: streamAttemptEvent{AttemptID: a.ID, StoredEvent: ev}})
				seqs.Advance(ev)
			}
		}
	}
	sort.SliceStable(items, func(i, j int) bool { return items[i].ts.Before(items[j].ts) })
	for _, it := range items {
		if it.event == "journal" {
			floor = it.id
		} else {
			it.id = floor
		}
		if err := writeSSE(w, it.event, it.id, it.data); err != nil {
			return 0, err
		}
	}
	return len(items), nil
}

// writeSSE frames one server-sent event. json.Marshal never emits newlines,
// so the data field is always a single line.
func writeSSE(w io.Writer, event string, id int64, v any) error {
	data, err := json.Marshal(v)
	if err != nil {
		return fmt.Errorf("encode %s event: %w", event, err)
	}
	if _, err := fmt.Fprintf(w, "event: %s\nid: %d\ndata: %s\n\n", event, id, data); err != nil {
		return fmt.Errorf("write %s event: %w", event, err)
	}
	return nil
}

// streamRetryHint tells the client to reconnect shortly: the daemon is
// draining or stopping and may be back after an exec (DESIGN.md §1.4).
func (s *Server) streamRetryHint(ctx context.Context, w io.Writer, rc *http.ResponseController) {
	if _, err := io.WriteString(w, "retry: 2000\n\n"); err != nil {
		s.log.DebugContext(ctx, "stream retry hint", "error", err)
	}
	if err := rc.Flush(); err != nil {
		s.log.DebugContext(ctx, "stream retry hint flush", "error", err)
	}
}
