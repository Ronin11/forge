package web

import (
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"syscall"
	"time"

	"forge/internal/core/daemon"
	"forge/internal/core/engine"
	"forge/internal/core/store"
)

// healthRoutes serves DESIGN.md §23's operational surface: GET /api/v1/health
// (what doctor and the status file consume) and POST /api/v1/backup (the
// daemon owns the database, so the CLI asks it to write the archive).
func (s *Server) healthRoutes(m *http.ServeMux) {
	m.HandleFunc("GET /api/v1/health", s.handle(s.health))
	m.HandleFunc("POST /api/v1/backup", s.handle(s.backup))
}

// healthBody is GET /api/v1/health. Ages are wall-clock seconds (they span
// processes; attrs.clock = "wall").
type healthBody struct {
	Daemon        string             `json:"daemon"` // ok | draining
	Version       string             `json:"version"`
	UptimeS       float64            `json:"uptime_s"`
	Schema        string             `json:"schema"`
	Worker        healthWorker       `json:"worker"`
	Plugins       []healthPlugin     `json:"plugins"`
	DiskFreeBytes uint64             `json:"disk_free_bytes"`
	Backup        healthBackupStatus `json:"backup"`
}

type healthWorker struct {
	Registered     bool     `json:"registered"`
	Connected      bool     `json:"connected"`
	HeartbeatAgeS  *float64 `json:"heartbeat_age_s,omitempty"`
	MaxConcurrent  int      `json:"max_concurrent,omitempty"`
	ActiveAttempts int      `json:"active_attempts,omitempty"`
}

type healthPlugin struct {
	Name    string `json:"name"`
	Running bool   `json:"running"`
}

type healthBackupStatus struct {
	LastAt *time.Time `json:"last_at,omitempty"`
	AgeS   *float64   `json:"age_s,omitempty"`
}

func (s *Server) health(r *http.Request) (int, any, error) {
	ctx := r.Context()
	now := s.now()
	body := healthBody{Daemon: "ok", Version: s.version, Schema: s.store.SchemaVersion()}
	if s.Draining() {
		body.Daemon = "draining"
	}
	if s.home != "" {
		if st, err := daemon.ReadState(s.home); err == nil && st != nil && !st.StartedAt.IsZero() {
			body.UptimeS = now.Sub(st.StartedAt).Seconds()
		}
		if path, mtime, err := engine.LatestBackup(filepath.Join(s.home, "backups")); err == nil && path != "" {
			age := now.Sub(mtime).Seconds()
			body.Backup = healthBackupStatus{LastAt: &mtime, AgeS: &age}
		}
		var fs syscall.Statfs_t
		if err := syscall.Statfs(s.home, &fs); err == nil {
			body.DiskFreeBytes = uint64(fs.Bavail) * uint64(fs.Bsize)
		}
	}
	workers, err := s.store.Workers(ctx, now)
	if err != nil {
		return 0, nil, err
	}
	if len(workers) > 0 {
		// The freshest worker speaks for the fleet of one this design runs.
		w := workers[0]
		for _, cand := range workers[1:] {
			if cand.LastSeenAt.After(w.LastSeenAt) {
				w = cand
			}
		}
		age := now.Sub(w.LastSeenAt).Seconds()
		body.Worker = healthWorker{Registered: true, Connected: w.Connected, HeartbeatAgeS: &age, MaxConcurrent: w.MaxConcurrent, ActiveAttempts: w.Active}
	}
	body.Plugins = []healthPlugin{}
	if s.pluginHealth != nil {
		for _, p := range s.pluginHealth() {
			body.Plugins = append(body.Plugins, healthPlugin{Name: p.Name, Running: p.Running})
		}
	}
	return http.StatusOK, body, nil
}

// backupRequest is POST /api/v1/backup. Out is the directory the archive
// lands in; empty means <home>/backups.
type backupRequest struct {
	Out string `json:"out"`
}

// backupResponse names the archive the daemon wrote.
type backupResponse struct {
	Archive string `json:"archive"`
	Bytes   int64  `json:"bytes"`
}

func (s *Server) backup(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.home == "" {
		return http.StatusNotImplemented, nil, fmt.Errorf("this daemon has no home directory; backup is unavailable")
	}
	var req backupRequest
	if r.ContentLength != 0 {
		if err := decodeJSON(r, &req); err != nil {
			return 0, nil, err
		}
	}
	out := req.Out
	if out == "" {
		out = filepath.Join(s.home, "backups")
	}
	if !filepath.IsAbs(out) {
		return 0, nil, badRequest("out %q: want an absolute directory", out)
	}
	archive, err := engine.WriteBackupArchive(ctx, s.store, engine.BackupInputs{Home: s.home, KbDir: s.kbDir, OutDir: out, Clock: s.now})
	if err != nil {
		return 0, nil, err
	}
	fi, err := os.Stat(archive)
	if err != nil {
		return 0, nil, fmt.Errorf("stat archive: %w", err)
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.Journal(ctx, "daemon.backup", store.EntityDaemon, "daemon", map[string]any{"archive": archive, "bytes": fi.Size()})
	}); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "backup written", "archive", archive, "bytes", fi.Size())
	return http.StatusOK, backupResponse{Archive: archive, Bytes: fi.Size()}, nil
}
