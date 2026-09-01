package main

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"

	"forge/internal/core/logging"
	"forge/internal/worker"
)

// runWorker is `forge worker start`: the long-lived worker process, run by the
// daemon as a detached child, by systemd, or by hand.
func runWorker(ctx context.Context, c *cmdContext, args []string) int {
	if len(args) == 0 || args[0] != "start" {
		fmt.Fprintln(c.stderr, "usage: forge worker start [--config PATH]")
		if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
			return 0
		}
		return 2
	}
	fs, lf := c.flags("worker start")
	cfgPath := fs.String("config", filepath.Join(c.forgeHome, "worker.toml"), "worker configuration")
	if code := c.parse(fs, args[1:]); code >= 0 {
		return code
	}
	self, err := os.Executable()
	if err != nil {
		fmt.Fprintln(c.stderr, "forge worker:", err)
		return 1
	}
	if _, err := worker.WriteDefault(*cfgPath, c.forgeHome, self); err != nil {
		fmt.Fprintln(c.stderr, "forge worker:", err)
		return 1
	}
	cfg, err := worker.LoadConfig(*cfgPath)
	if err != nil {
		fmt.Fprintln(c.stderr, "forge worker:", err)
		return 1
	}
	opts, err := logging.Resolve(lf, c.getenv, cfg.Log, c.forgeHome)
	if err != nil {
		fmt.Fprintln(c.stderr, "forge worker:", err)
		return 2
	}
	sink, err := logging.OpenFileSink("worker", opts.File)
	if err != nil {
		fmt.Fprintln(c.stderr, "forge worker:", err)
		return 1
	}
	defer func() {
		if err := sink.Close(); err != nil {
			fmt.Fprintln(c.stderr, "forge worker: close log:", err)
		}
	}()
	handler := logging.New(c.stderr, opts, sink)
	log := handler.For("worker")
	ctx, stop := signal.NotifyContext(ctx, syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	w, err := worker.New(ctx, worker.WorkerOptions{Config: cfg, Version: version, Handler: handler, ForgeBin: self})
	if err != nil {
		log.ErrorContext(ctx, "worker cannot start", "error", err)
		fmt.Fprintln(c.stderr, "forge worker:", err)
		return 1
	}
	defer func() {
		if err := w.Close(); err != nil {
			log.WarnContext(ctx, "release data dir lock", "error", err)
		}
	}()
	if err := w.Run(ctx); err != nil {
		log.ErrorContext(ctx, "worker exited with error", "error", err)
		return 1
	}
	return 0
}
