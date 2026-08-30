package logging

import (
	"context"
	"io"
	"log/slog"
)

// Handler is the one slog.Handler in a Forge process. It fans each record out to
// its sinks — stderr, governed by the operator's per-component levels, and an
// optional file, always JSON at debug — and stamps the component name and the
// correlation attrs carried by the context. Package loggers come from For.
type Handler struct {
	levels *levelStore
	format string
	sinks  []sink
}

// sink is one destination and the rule for whether a record reaches it.
type sink struct {
	handler slog.Handler
	// accept decides from the component and level; the stderr sink consults the
	// live Levels, the file sink is fixed at debug.
	accept func(levels Levels, component string, level slog.Level) bool
}

// New builds the process handler. stderr receives records at the configured
// levels in the configured format; file, when non-nil, receives JSON at debug and
// above regardless of the flags. A process has at most one file sink, named for
// the process (daemon, worker); two processes never share one file because the
// daemon lock and the worker data-directory lock forbid two of either.
func New(stderr io.Writer, opts Options, file io.Writer) *Handler {
	h := &Handler{levels: &levelStore{current: opts.Levels.clone()}, format: opts.Format}
	h.sinks = append(h.sinks, sink{
		handler: newTextOrJSON(stderr, opts.Format),
		accept: func(levels Levels, component string, level slog.Level) bool {
			return level >= levels.For(component)
		},
	})
	if file != nil {
		h.sinks = append(h.sinks, sink{
			handler: newTextOrJSON(file, FormatJSON),
			accept: func(_ Levels, _ string, level slog.Level) bool {
				return level >= slog.LevelDebug
			},
		})
	}
	return h
}

// Discard is a handler for tests and for commands that have not resolved their
// options yet; it accepts nothing.
func Discard() *Handler {
	return New(io.Discard, Options{Levels: Levels{Default: slog.LevelError}, Format: FormatText}, nil)
}

// newTextOrJSON builds the underlying slog handler at the lowest level; the
// Forge handler decides what passes, so the inner one must never filter.
func newTextOrJSON(w io.Writer, format string) slog.Handler {
	opts := &slog.HandlerOptions{Level: LevelTrace, ReplaceAttr: renderLevel}
	if format == FormatJSON {
		return slog.NewJSONHandler(w, opts)
	}
	return slog.NewTextHandler(w, opts)
}

// renderLevel makes the custom trace level print as TRACE, not DEBUG-4.
func renderLevel(groups []string, a slog.Attr) slog.Attr {
	if a.Key == slog.LevelKey && len(groups) == 0 {
		if l, ok := a.Value.Any().(slog.Level); ok {
			return slog.String(slog.LevelKey, LevelName(l))
		}
	}
	return a
}

// For returns the logger a package uses. component is a dotted name
// ("worker.git", "controlplane.http", "store"); it is both the level-lookup key
// and the "component" attribute on every line. It is fixed at construction:
// logger.With("component", …) would add a second key and change nothing, so
// don't — ask For for another logger instead.
func (h *Handler) For(component string) *slog.Logger {
	per := make([]slog.Handler, len(h.sinks))
	for i, s := range h.sinks {
		per[i] = s.handler.WithAttrs([]slog.Attr{slog.String("component", component)})
	}
	return slog.New(&componentHandler{parent: h, component: component, sinks: per})
}

// Levels returns the live levels (for the API and for children to inherit).
func (h *Handler) Levels() Levels { return h.levels.get() }

// Format is the stderr format, which children inherit.
func (h *Handler) Format() string { return h.format }

// SetLevels replaces the live levels; takes effect on the next record.
func (h *Handler) SetLevels(l Levels) { h.levels.set(l) }

// ToggleDebug is the SIGUSR1 behaviour: debug on, or back to the previous level.
func (h *Handler) ToggleDebug() Levels { return h.levels.toggleDebug() }

// componentHandler is what a package logger holds: the parent's sinks with the
// component attr pre-applied, plus the component name for level lookup.
type componentHandler struct {
	parent    *Handler
	component string
	sinks     []slog.Handler // parallel to parent.sinks
}

func (c *componentHandler) Enabled(_ context.Context, level slog.Level) bool {
	levels := c.parent.levels.get()
	for _, s := range c.parent.sinks {
		if s.accept(levels, c.component, level) {
			return true
		}
	}
	return false
}

func (c *componentHandler) Handle(ctx context.Context, r slog.Record) error {
	if attrs := AttrsFrom(ctx); len(attrs) > 0 {
		r = r.Clone()
		r.AddAttrs(attrs...)
	}
	levels := c.parent.levels.get()
	var firstErr error
	for i, s := range c.parent.sinks {
		if !s.accept(levels, c.component, r.Level) {
			continue
		}
		if err := c.sinks[i].Handle(ctx, r); err != nil && firstErr == nil {
			firstErr = err
		}
	}
	return firstErr
}

func (c *componentHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	out := &componentHandler{parent: c.parent, component: c.component, sinks: make([]slog.Handler, len(c.sinks))}
	for i, s := range c.sinks {
		out.sinks[i] = s.WithAttrs(attrs)
	}
	return out
}

// WithGroup is deliberately a no-op. Correlation attrs are stamped at Handle time
// and must stay top-level so "every line inside an attempt has attempt_id" is a
// grep, not a tree walk; a group would nest them. Forge logs flat records.
func (c *componentHandler) WithGroup(string) slog.Handler { return c }
