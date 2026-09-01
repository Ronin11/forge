package logging

import (
	"context"
	"log/slog"
	"os"
	"os/signal"
	"syscall"
)

// SignalWatcher toggles debug on a handler when the process receives SIGUSR1.
// Registration happens in WatchSIGUSR1, synchronously, so a signal arriving after
// the constructor returns is never missed (and never kills the process); Run
// then services them until ctx is done. Long-running commands (daemon, worker)
// run one; one-shot CLI commands do not.
type SignalWatcher struct {
	handler  *Handler
	logger   *slog.Logger
	onChange func(Levels)
	signals  chan os.Signal
}

// WatchSIGUSR1 registers for SIGUSR1 immediately. onChange, if non-nil, is
// called with the new levels so the process can propagate them to children.
func WatchSIGUSR1(h *Handler, logger *slog.Logger, onChange func(Levels)) *SignalWatcher {
	w := &SignalWatcher{handler: h, logger: logger, onChange: onChange, signals: make(chan os.Signal, 1)}
	signal.Notify(w.signals, syscall.SIGUSR1)
	return w
}

// Run services signals until ctx is done, then unregisters.
func (w *SignalWatcher) Run(ctx context.Context) {
	defer signal.Stop(w.signals)
	for {
		select {
		case <-ctx.Done():
			return
		case <-w.signals:
			levels := w.handler.ToggleDebug()
			w.logger.InfoContext(ctx, "log levels changed", "trigger", "SIGUSR1", "levels", levels.String())
			if w.onChange != nil {
				w.onChange(levels)
			}
		}
	}
}
