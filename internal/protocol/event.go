// Package protocol holds the wire types shared by the daemon, the worker, forge
// mcp, plugins, and the CLI. It contains no logic beyond validation and imports
// nothing from Forge except model, so every side of an HTTP call decodes the same
// struct.
package protocol

import (
	"encoding/json"
	"fmt"
	"time"
)

// Event kinds. Only stdout/stderr are subject to the per-attempt cap.
const (
	KindLifecycle = "lifecycle"
	KindStdout    = "stdout"
	KindStderr    = "stderr"
	KindSpanStart = "span_start"
	KindSpanEnd   = "span_end"
	KindMetric    = "metric"
)

// Event sources; each numbers its own seq so batches never collide.
const (
	SourceWorker  = "worker"
	SourceMCP     = "mcp"
	SourceControl = "control"
)

// Event is one span, metric, or line of an attempt's timeline (DESIGN.md §8).
type Event struct {
	Seq        int             `json:"seq"`
	Time       time.Time       `json:"time"`
	ElapsedUS  int64           `json:"elapsed_us"`
	Kind       string          `json:"kind"`
	Message    string          `json:"message"`
	SpanID     string          `json:"span_id,omitempty"`
	ParentID   string          `json:"parent_id,omitempty"`
	Name       string          `json:"name,omitempty"`
	DurationUS int64           `json:"duration_us,omitempty"`
	Attrs      json.RawMessage `json:"attrs,omitempty"`
}

// Validate rejects malformed events before they reach the store.
func (e Event) Validate() error {
	switch e.Kind {
	case KindLifecycle, KindStdout, KindStderr, KindSpanStart, KindSpanEnd, KindMetric:
	default:
		return fmt.Errorf("event %d: unknown kind %q", e.Seq, e.Kind)
	}
	if e.Seq < 0 || e.ElapsedUS < 0 || e.DurationUS < 0 {
		return fmt.Errorf("event %d: negative seq, elapsed, or duration", e.Seq)
	}
	if (e.Kind == KindSpanStart || e.Kind == KindSpanEnd) && (e.SpanID == "" || e.Name == "") {
		return fmt.Errorf("event %d: span without id or name", e.Seq)
	}
	if len(e.Attrs) > MaxAttrsBytes {
		return fmt.Errorf("event %d: attrs %d bytes exceed %d", e.Seq, len(e.Attrs), MaxAttrsBytes)
	}
	return nil
}

// Limits every side enforces identically.
const (
	MaxAttrsBytes       = 4 << 10
	MaxEventBatch       = 100
	MaxEventBatchBytes  = 256 << 10
	MaxLineEventBytes   = 8 << 10   // one stdout/stderr event's message
	MaxLineEvents       = 2000      // stdout+stderr events per attempt
	MaxLineEventTotal   = 1 << 20   // bytes of stdout+stderr events per attempt
	MaxResultTextBytes  = 256 << 10 // result text stored on the attempt
	MaxResultJSONBytes  = 256 << 10 // structured result
	MaxPromptBytes      = 64 << 10
	MaxRepositoriesWork = 100
)

// EventBatch is the body of POST /api/v1/attempts/{id}/events.
type EventBatch struct {
	Source string  `json:"source"`
	Events []Event `json:"events"`
}
