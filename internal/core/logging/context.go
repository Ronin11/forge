package logging

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"log/slog"
)

type ctxKey struct{}

// ContextWith returns a context whose log lines carry attrs. Correlation fields
// (request_id, attempt_id, target_id, work_id, span_id, plugin) travel this way
// so call sites never repeat them: the handler stamps them on every record logged
// with a *Context method. Attrs accumulate; a later value for the same key wins.
func ContextWith(ctx context.Context, attrs ...slog.Attr) context.Context {
	if len(attrs) == 0 {
		return ctx
	}
	existing := AttrsFrom(ctx)
	merged := make([]slog.Attr, 0, len(existing)+len(attrs))
	merged = append(merged, existing...)
	for _, a := range attrs {
		merged = replaceOrAppend(merged, a)
	}
	return context.WithValue(ctx, ctxKey{}, merged)
}

// AttrsFrom returns the correlation attrs carried by ctx; a nil or bare context
// yields nil, so call sites need no guard.
func AttrsFrom(ctx context.Context) []slog.Attr {
	if ctx == nil {
		return nil
	}
	attrs, ok := ctx.Value(ctxKey{}).([]slog.Attr)
	if !ok {
		return nil
	}
	return attrs
}

// HasAttr reports whether ctx carries key; tests use it to enforce the rule that
// nothing inside an attempt logs without attempt_id.
func HasAttr(ctx context.Context, key string) bool {
	for _, a := range AttrsFrom(ctx) {
		if a.Key == key {
			return true
		}
	}
	return false
}

func replaceOrAppend(attrs []slog.Attr, a slog.Attr) []slog.Attr {
	for i := range attrs {
		if attrs[i].Key == a.Key {
			attrs[i] = a
			return attrs
		}
	}
	return append(attrs, a)
}

// NewRequestID is 16 hex characters of entropy: enough to be unique in a log,
// short enough to grep by hand. It is not a Forge ID (those are 32 hex).
func NewRequestID() string {
	var b [8]byte
	if _, err := rand.Read(b[:]); err != nil {
		// crypto/rand failing is not a condition Forge can work around; the
		// request is still served, only its correlation id is degraded.
		return "rand-unavailable"
	}
	return hex.EncodeToString(b[:])
}
