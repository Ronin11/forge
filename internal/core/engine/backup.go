// Package-shared backup machinery (MODULARIZATION.md §6): the web backup
// endpoint and forge backup/restore both drive these.
package engine

import (
	"archive/tar"
	"compress/gzip"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"forge/internal/core/daemon"
	"forge/internal/core/store"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// BackupInputs are what WriteBackupArchive cannot derive from the store.
type BackupInputs struct {
	Home   string           // the Forge home; config.toml, worker.toml, modes/ live here
	KbDir  string           // [kb] path; empty means <home>/kb
	OutDir string           // where the archive lands (created if absent)
	Clock  func() time.Time // defaults to time.Now
}

// backupTimeFormat names archives so lexical order is creation order.
const backupTimeFormat = "20060102T150405Z"

// BackupPrefix is the archive filename prefix retention and health key on.
const BackupPrefix = "forge-backup-"

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
	if err := st.BackupInto(ctx, filepath.Join(staging, daemon.DBFile)); err != nil {
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
	archive = filepath.Join(in.OutDir, BackupPrefix+ts+".tar.gz")
	if err := TarGzDir(staging, archive); err != nil {
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
		if !e.IsDir() && strings.HasPrefix(e.Name(), BackupPrefix) && strings.HasSuffix(e.Name(), ".tar.gz") {
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

// TarGzDir packs dir's contents (paths relative to dir) into archive.
func TarGzDir(dir, archive string) (err error) {
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
