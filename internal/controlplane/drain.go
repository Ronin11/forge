package controlplane

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"time"

	"forge/internal/core/store"
)

// drainBody is POST /api/v1/daemon/drain's optional body (DESIGN.md §1.4).
// Without it the drain is plain: stop admitting, keep the worker paths open.
// With exec, once in-flight requests finish (up to the timeout) the daemon
// execs the named binary with the lock and listener descriptors inherited.
type drainBody struct {
	Exec           string `json:"exec"`
	TimeoutSeconds int    `json:"timeout_seconds"`
}

func (s *Server) drain(r *http.Request) (int, any, error) {
	ctx := r.Context()
	var body drainBody
	raw, err := io.ReadAll(r.Body)
	if err != nil {
		return 0, nil, badRequest("read body: %v", err)
	}
	if len(bytes.TrimSpace(raw)) > 0 {
		if err := json.Unmarshal(raw, &body); err != nil {
			return 0, nil, badRequest("decode body: %v", err)
		}
	}
	if body.Exec != "" {
		if err := validateExec(body.Exec); err != nil {
			return 0, nil, err
		}
		if s.execRestart == nil {
			return 0, nil, badRequest("this daemon cannot exec-restart")
		}
	}
	timeout := time.Duration(body.TimeoutSeconds) * time.Second
	if timeout <= 0 {
		timeout = 30 * time.Second
	}
	s.SetDraining(true)
	payload := map[string]any{"actor": "human"}
	if body.Exec != "" {
		payload["exec"] = body.Exec
		payload["timeout_seconds"] = int(timeout.Seconds())
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.Journal(ctx, "daemon.draining", store.EntityDaemon, "daemon", payload)
	})
	if err != nil {
		return 0, nil, err
	}
	if s.home != "" {
		if err := s.markStateDraining(); err != nil {
			s.log.WarnContext(ctx, "update daemon.json", "error", err)
		}
	}
	s.log.InfoContext(ctx, "draining", "exec", body.Exec)
	if body.Exec != "" {
		// This goroutine's owner is the process image itself: on success exec
		// never returns, and on failure it logs and leaves the daemon
		// draining, so there is nothing for a WaitGroup to wait for.
		bg := context.WithoutCancel(ctx)
		execPath := body.Exec
		go func() {
			s.waitInflightIdle(bg, timeout)
			s.log.InfoContext(bg, "restarting via exec", "exec", execPath)
			if err := s.execRestart(execPath); err != nil {
				s.log.ErrorContext(bg, "exec restart failed; the daemon stays draining", "exec", execPath, "error", err)
			}
		}()
	}
	return http.StatusOK, map[string]string{"state": "draining"}, nil
}

// validateExec enforces §1.4's rule on the drain body: the binary to exec must
// be an absolute path to a regular executable file, checked before any drain
// side effect so a typo'd restart refuses cleanly.
func validateExec(path string) error {
	if !filepath.IsAbs(path) {
		return badRequest("exec %q: want an absolute path", path)
	}
	fi, err := os.Stat(path)
	if err != nil {
		return badRequest("exec %s: %v", path, err)
	}
	if !fi.Mode().IsRegular() {
		return badRequest("exec %s: not a regular file", path)
	}
	if fi.Mode().Perm()&0o111 == 0 {
		return badRequest("exec %s: not executable", path)
	}
	return nil
}

// waitInflightIdle blocks until no request is inside handle or the timeout
// passes — the quiet moment §1.4's exec wants. The drain request itself has
// finished writing its response by the time the counter reaches zero, because
// handle decrements only after the body is out.
func (s *Server) waitInflightIdle(ctx context.Context, timeout time.Duration) {
	deadline := time.Now().Add(timeout)
	for s.inflight.Load() != 0 && time.Now().Before(deadline) {
		select {
		case <-ctx.Done():
			return
		case <-time.After(25 * time.Millisecond):
		}
	}
}

// markStateDraining rewrites daemon.json's state so `daemon status` agrees with
// the handshake; a missing file is left missing.
func (s *Server) markStateDraining() error {
	st, err := ReadState(s.home)
	if err != nil || st == nil {
		return err
	}
	st.State = "draining"
	return WriteState(s.home, *st)
}
