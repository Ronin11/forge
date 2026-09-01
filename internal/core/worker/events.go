package worker

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"forge/internal/core/protocol"
)

// Batch limits (DESIGN.md §8): flush every 500 ms, or at 100 events, or at
// 256 KiB, whichever first.
const (
	flushInterval = 500 * time.Millisecond
	sendRetryMin  = 100 * time.Millisecond
	sendRetryMax  = 2 * time.Second
)

// eventSink is where an emitter delivers batches (the daemon client in
// production, a recorder in tests).
type eventSink interface {
	Events(ctx context.Context, attemptID string, batch protocol.EventBatch) error
}

// Emitter assigns seq/time/elapsed to events, caps stdout/stderr, and ships
// batches. One per launch; elapsedBefore and nextSeq carry across launches so
// a resumed attempt continues its timeline instead of colliding with it.
type Emitter struct {
	attemptID string
	sink      eventSink
	log       *slog.Logger
	clock     func() time.Time
	start     time.Time // monotonic anchor for this launch
	before    int64     // elapsed_us accumulated by earlier launches

	mu         sync.Mutex // guards everything below
	seq        int
	pending    []protocol.Event
	pendingLen int
	lineCount  int
	lineBytes  int64
	dropped    int
	droppedAt  bool // the events_dropped lifecycle event was emitted
	disabled   bool // a 4xx: the daemon refuses this attempt's events
	flushed    chan struct{}
}

// NewEmitter starts the timeline at now; nextSeq and elapsedBefore come from the
// manifest on a resume, zero on the first launch.
func NewEmitter(attemptID string, sink eventSink, log *slog.Logger, clock func() time.Time, nextSeq int, elapsedBefore int64) *Emitter {
	return &Emitter{attemptID: attemptID, sink: sink, log: log, clock: clock, start: time.Now(), before: elapsedBefore, seq: nextSeq, flushed: make(chan struct{}, 1)}
}

// Elapsed is the attempt's monotonic clock: microseconds since the first
// launch, excluding time spent paused between launches.
func (e *Emitter) Elapsed() int64 { return e.before + time.Since(e.start).Microseconds() }

// NextSeq is what the manifest records for the next launch.
func (e *Emitter) NextSeq() int {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.seq
}

// Dropped reports how many stdout/stderr events the cap discarded.
func (e *Emitter) Dropped() int {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.dropped
}

// Emit stamps and queues one event. stdout/stderr events beyond the per-attempt
// cap are counted and dropped; everything else is never dropped.
func (e *Emitter) Emit(ev protocol.Event) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if ev.Kind == protocol.KindStdout || ev.Kind == protocol.KindStderr {
		if len(ev.Message) > protocol.MaxLineEventBytes {
			ev.Message = ev.Message[:protocol.MaxLineEventBytes]
		}
		if e.lineCount >= protocol.MaxLineEvents || e.lineBytes+int64(len(ev.Message)) > protocol.MaxLineEventTotal {
			e.dropped++
			if !e.droppedAt {
				e.droppedAt = true
				e.queueLocked(protocol.Event{Kind: protocol.KindLifecycle, Name: "events_dropped", Message: "stdout/stderr cap reached; further lines are counted only", Attrs: json.RawMessage(`{"dropped":1}`)})
			}
			return
		}
		e.lineCount++
		e.lineBytes += int64(len(ev.Message))
	}
	e.queueLocked(ev)
}

func (e *Emitter) queueLocked(ev protocol.Event) {
	ev.Seq = e.seq
	e.seq++
	ev.Time = e.clock().UTC()
	ev.ElapsedUS = e.Elapsed()
	e.pending = append(e.pending, ev)
	e.pendingLen += len(ev.Message) + len(ev.Attrs) + 64
	if len(e.pending) >= protocol.MaxEventBatch || e.pendingLen >= protocol.MaxEventBatchBytes {
		select {
		case e.flushed <- struct{}{}:
		default:
		}
	}
}

// Lifecycle emits a lifecycle event.
func (e *Emitter) Lifecycle(message string, attrs any) {
	e.Emit(protocol.Event{Kind: protocol.KindLifecycle, Message: message, Attrs: marshalAttrs(attrs)})
}

// Metric emits a metric event.
func (e *Emitter) Metric(name string, attrs any) {
	e.Emit(protocol.Event{Kind: protocol.KindMetric, Name: name, Message: name, Attrs: marshalAttrs(attrs)})
}

// Span is an open phase or tool span.
type Span struct {
	e       *Emitter
	id      string
	name    string
	parent  string
	started int64
}

// StartSpan opens a span whose id is the name for phases (unique per launch)
// or whatever the caller chooses.
func (e *Emitter) StartSpan(id, name, parent string, attrs any) *Span {
	s := &Span{e: e, id: id, name: name, parent: parent, started: e.Elapsed()}
	e.Emit(protocol.Event{Kind: protocol.KindSpanStart, SpanID: id, ParentID: parent, Name: name, Message: name, Attrs: marshalAttrs(attrs)})
	return s
}

// End closes the span with its monotonic duration; err, when non-nil, lands in
// attrs.error before the lifecycle event that reports it.
func (s *Span) End(err error, attrs map[string]any) {
	if attrs == nil {
		attrs = map[string]any{}
	}
	if err != nil {
		attrs["error"] = shortError(err)
	}
	s.e.Emit(protocol.Event{Kind: protocol.KindSpanEnd, SpanID: s.id, ParentID: s.parent, Name: s.name, Message: s.name, DurationUS: s.e.Elapsed() - s.started, Attrs: marshalAttrs(attrs)})
}

// EndTool closes a tool span opened by the parser; its duration is computed
// from the recorded start elapsed.
func (e *Emitter) EndTool(ev protocol.Event, startedAt int64) {
	ev.DurationUS = e.Elapsed() - startedAt
	e.Emit(ev)
}

func marshalAttrs(v any) json.RawMessage {
	if v == nil {
		return nil
	}
	b, err := json.Marshal(v)
	if err != nil || len(b) > protocol.MaxAttrsBytes {
		return json.RawMessage(`{"attrs":"unrepresentable"}`)
	}
	return b
}

func shortError(err error) string {
	s := err.Error()
	if len(s) > 200 {
		return s[:200]
	}
	return s
}

// Run ships batches until ctx is done, then flushes what remains (bounded by
// the final-flush deadline the caller puts on ctx's successor via Flush).
func (e *Emitter) Run(ctx context.Context) {
	ticker := time.NewTicker(flushInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		case <-e.flushed:
		}
		e.flush(ctx)
	}
}

// Flush sends everything pending; the final flush happens before complete.
func (e *Emitter) Flush(ctx context.Context) { e.flush(ctx) }

func (e *Emitter) flush(ctx context.Context) {
	e.mu.Lock()
	if len(e.pending) == 0 || e.disabled {
		e.mu.Unlock()
		return
	}
	batch := e.pending
	e.pending, e.pendingLen = nil, 0
	e.mu.Unlock()
	delay := sendRetryMin
	for {
		err := e.sink.Events(ctx, e.attemptID, protocol.EventBatch{Source: protocol.SourceWorker, Events: batch})
		if err == nil {
			return
		}
		var se *StatusError
		if errors.As(err, &se) && se.Status >= 400 && se.Status < 500 {
			e.log.WarnContext(ctx, "daemon refused events; disabling further sends", "status", se.Status, "message", se.Message)
			e.mu.Lock()
			e.disabled = true
			e.mu.Unlock()
			return
		}
		e.log.DebugContext(ctx, "events send failed; retrying", "error", err, "delay", delay)
		select {
		case <-ctx.Done():
			// Put the batch back so a later flush with a fresh context can try again.
			e.mu.Lock()
			e.pending = append(batch, e.pending...)
			e.mu.Unlock()
			return
		case <-time.After(delay):
		}
		if delay *= 2; delay > sendRetryMax {
			delay = sendRetryMax
		}
	}
}

// PendingCount is for tests.
func (e *Emitter) PendingCount() int {
	e.mu.Lock()
	defer e.mu.Unlock()
	return len(e.pending)
}

// String renders the emitter for logs.
func (e *Emitter) String() string { return fmt.Sprintf("emitter(%s)", e.attemptID) }
