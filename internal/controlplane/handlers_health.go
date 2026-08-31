package controlplane

import (
	"archive/tar"
	"compress/gzip"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"

	"forge/internal/store"
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
		if st, err := ReadState(s.home); err == nil && st != nil && !st.StartedAt.IsZero() {
			body.UptimeS = now.Sub(st.StartedAt).Seconds()
		}
		if path, mtime, err := LatestBackup(filepath.Join(s.home, "backups")); err == nil && path != "" {
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
	archive, err := WriteBackupArchive(ctx, s.store, BackupInputs{Home: s.home, KbDir: s.kbDir, OutDir: out, Clock: s.now})
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

// BackupInputs are what WriteBackupArchive cannot derive from the store.
type BackupInputs struct {
	Home   string           // the Forge home; config.toml, worker.toml, modes/ live here
	KbDir  string           // [kb] path; empty means <home>/kb
	OutDir string           // where the archive lands (created if absent)
	Clock  func() time.Time // defaults to time.Now
}

// backupTimeFormat names archives so lexical order is creation order.
const backupTimeFormat = "20060102T150405Z"

// backupPrefix is the archive filename prefix retention and health key on.
const backupPrefix = "forge-backup-"

// WriteBackupArchive writes one self-contained backup: the database via
// VACUUM INTO, plus kb/, modes/, config.toml, worker.toml, and the plugins
// table exported as plugins.json, staged in a directory and then packed as
// OutDir/forge-backup-<UTC timestamp>.tar.gz (the staging directory is
// removed — backups are single files). The nightly loop and POST
// /api/v1/backup both call this; the rule lives once.
func WriteBackupArchive(ctx context.Context, st *store.Store, in BackupInputs) (archive string, err error) {
	if in.Clock == nil {
		in.Clock = time.Now
	}
	if in.KbDir == "" {
		in.KbDir = filepath.Join(in.Home, "kb")
	}
	ts := in.Clock().UTC().Format(backupTimeFormat)
	staging := filepath.Join(in.OutDir, "forge-"+ts)
	if err := os.MkdirAll(staging, 0o700); err != nil {
		return "", fmt.Errorf("backup: create %s: %w", staging, err)
	}
	defer func() {
		if rerr := os.RemoveAll(staging); rerr != nil {
			err = errors.Join(err, fmt.Errorf("remove staging %s: %w", staging, rerr))
		}
	}()
	if err := st.BackupInto(ctx, filepath.Join(staging, DBFile)); err != nil {
		return "", err
	}
	for _, name := range []string{"config.toml", "worker.toml"} {
		if err := copyFileIfExists(filepath.Join(in.Home, name), filepath.Join(staging, name)); err != nil {
			return "", fmt.Errorf("backup %s: %w", name, err)
		}
	}
	for src, dst := range map[string]string{in.KbDir: "kb", filepath.Join(in.Home, "modes"): "modes"} {
		if err := copyDirIfExists(src, filepath.Join(staging, dst)); err != nil {
			return "", fmt.Errorf("backup %s: %w", dst, err)
		}
	}
	plugins, err := st.Plugins(ctx)
	if err != nil {
		return "", fmt.Errorf("backup plugins: %w", err)
	}
	pb, err := json.MarshalIndent(plugins, "", "  ")
	if err != nil {
		return "", fmt.Errorf("backup: encode plugins: %w", err)
	}
	if err := os.WriteFile(filepath.Join(staging, "plugins.json"), pb, 0o600); err != nil {
		return "", fmt.Errorf("backup: write plugins.json: %w", err)
	}
	archive = filepath.Join(in.OutDir, backupPrefix+ts+".tar.gz")
	if err := tarGzDir(staging, archive); err != nil {
		return "", err
	}
	return archive, nil
}

// PruneBackups keeps the newest keep archives under dir and removes the rest.
// Archive names embed their UTC timestamp, so lexical order is age order.
func PruneBackups(dir string, keep int) (removed []string, err error) {
	if keep < 1 {
		return nil, fmt.Errorf("prune backups: keep %d: want at least 1", keep)
	}
	names, err := backupArchives(dir)
	if err != nil || len(names) <= keep {
		return nil, err
	}
	for _, name := range names[:len(names)-keep] {
		path := filepath.Join(dir, name)
		if rerr := os.Remove(path); rerr != nil {
			return removed, fmt.Errorf("prune backup %s: %w", path, rerr)
		}
		removed = append(removed, path)
	}
	return removed, nil
}

// LatestBackup reports the newest archive under dir; ("" , zero, nil) when
// there is none (a missing directory counts as none).
func LatestBackup(dir string) (path string, mtime time.Time, err error) {
	names, err := backupArchives(dir)
	if err != nil || len(names) == 0 {
		return "", time.Time{}, err
	}
	path = filepath.Join(dir, names[len(names)-1])
	fi, err := os.Stat(path)
	if err != nil {
		return "", time.Time{}, fmt.Errorf("stat backup %s: %w", path, err)
	}
	return path, fi.ModTime(), nil
}

// backupArchives lists forge-backup-*.tar.gz under dir, sorted ascending.
func backupArchives(dir string) ([]string, error) {
	entries, err := os.ReadDir(dir)
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read backups %s: %w", dir, err)
	}
	var names []string
	for _, e := range entries {
		if !e.IsDir() && strings.HasPrefix(e.Name(), backupPrefix) && strings.HasSuffix(e.Name(), ".tar.gz") {
			names = append(names, e.Name())
		}
	}
	sort.Strings(names)
	return names, nil
}

// copyFileIfExists copies a regular file, mode preserved as 0600; a missing
// source is fine (a fresh home may lack worker.toml).
func copyFileIfExists(src, dst string) (err error) {
	in, err := os.Open(src)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, in.Close()) }()
	out, err := os.OpenFile(dst, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o600)
	if err != nil {
		return err
	}
	if _, cerr := io.Copy(out, in); cerr != nil {
		return errors.Join(cerr, out.Close())
	}
	return out.Close()
}

// copyDirIfExists copies a tree of regular files (symlinks and specials are
// skipped — a kb of notes has neither); a missing source is fine.
func copyDirIfExists(src, dst string) error {
	if _, err := os.Stat(src); os.IsNotExist(err) {
		return nil
	} else if err != nil {
		return err
	}
	return filepath.WalkDir(src, func(path string, d os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(src, path)
		if err != nil {
			return err
		}
		target := filepath.Join(dst, rel)
		if d.IsDir() {
			return os.MkdirAll(target, 0o700)
		}
		if !d.Type().IsRegular() {
			return nil
		}
		return copyFileIfExists(path, target)
	})
}

// tarGzDir packs dir's contents (paths relative to dir) into archive.
func tarGzDir(dir, archive string) (err error) {
	f, err := os.OpenFile(archive, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o600)
	if err != nil {
		return fmt.Errorf("create archive %s: %w", archive, err)
	}
	defer func() { err = errors.Join(err, f.Close()) }()
	gz := gzip.NewWriter(f)
	tw := tar.NewWriter(gz)
	err = filepath.WalkDir(dir, func(path string, d os.DirEntry, werr error) error {
		if werr != nil {
			return werr
		}
		rel, rerr := filepath.Rel(dir, path)
		if rerr != nil {
			return rerr
		}
		if rel == "." {
			return nil
		}
		fi, ierr := d.Info()
		if ierr != nil {
			return ierr
		}
		hdr, herr := tar.FileInfoHeader(fi, "")
		if herr != nil {
			return herr
		}
		hdr.Name = filepath.ToSlash(rel)
		if d.IsDir() {
			hdr.Name += "/"
		}
		if terr := tw.WriteHeader(hdr); terr != nil {
			return terr
		}
		if d.IsDir() {
			return nil
		}
		src, oerr := os.Open(path)
		if oerr != nil {
			return oerr
		}
		if _, cerr := io.Copy(tw, src); cerr != nil {
			return errors.Join(cerr, src.Close())
		}
		return src.Close()
	})
	if err != nil {
		return fmt.Errorf("pack %s: %w", archive, err)
	}
	if err := tw.Close(); err != nil {
		return fmt.Errorf("finish tar %s: %w", archive, err)
	}
	if err := gz.Close(); err != nil {
		return fmt.Errorf("finish gzip %s: %w", archive, err)
	}
	return nil
}

// UnpackBackup extracts an archive WriteBackupArchive wrote into home,
// refusing entries that would escape it. It is the restore side of the pair;
// the CLI guards that home is fresh before calling.
func UnpackBackup(archive, home string) (err error) {
	f, err := os.Open(archive)
	if err != nil {
		return fmt.Errorf("open archive %s: %w", archive, err)
	}
	defer func() { err = errors.Join(err, f.Close()) }()
	gz, err := gzip.NewReader(f)
	if err != nil {
		return fmt.Errorf("read archive %s: %w", archive, err)
	}
	defer func() { err = errors.Join(err, gz.Close()) }()
	tr := tar.NewReader(gz)
	for {
		hdr, rerr := tr.Next()
		if rerr == io.EOF {
			return nil
		}
		if rerr != nil {
			return fmt.Errorf("read archive %s: %w", archive, rerr)
		}
		name := filepath.FromSlash(hdr.Name)
		if filepath.IsAbs(name) || !filepath.IsLocal(name) {
			return fmt.Errorf("archive entry %q escapes the home directory", hdr.Name)
		}
		target := filepath.Join(home, name)
		switch hdr.Typeflag {
		case tar.TypeDir:
			if merr := os.MkdirAll(target, 0o700); merr != nil {
				return merr
			}
		case tar.TypeReg:
			if merr := os.MkdirAll(filepath.Dir(target), 0o700); merr != nil {
				return merr
			}
			out, oerr := os.OpenFile(target, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o600)
			if oerr != nil {
				return oerr
			}
			// Size is unbounded by design: backups are operator-made archives
			// of the operator's own home, extracted into a fresh home.
			if _, cerr := io.Copy(out, tr); cerr != nil {
				return errors.Join(fmt.Errorf("extract %s: %w", hdr.Name, cerr), out.Close())
			}
			if cerr := out.Close(); cerr != nil {
				return cerr
			}
		default:
			return fmt.Errorf("archive entry %q has unsupported type %d", hdr.Name, hdr.Typeflag)
		}
	}
}
