package engine

import (
	"compress/gzip"
	"context"
	"errors"
	"fmt"
	"forge/internal/core/config"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// PruneInput drives one retention pass (DESIGN.md §9.3): raw output and
// artifacts age out; facts, spans, samples, and prompt versions are forever
// and are never touched here.
type PruneInput struct {
	DataDir   string
	Retention config.RetentionConfig
	Now       time.Time
	Delete    bool // false = dry run
	Logger    *slog.Logger
}

// PruneReport says what a pass did (or would do).
type PruneReport struct {
	Outputs      int
	OutputBytes  int64
	ArtifactDirs int
	Compressed   int
}

// Prune walks output/ and artifacts/ under the worker data dir. Output logs
// older than OutputDays are removed; logs older than 7 days are gzipped in
// place first (transcripts keep their .log.gz for TranscriptDays). Artifacts
// age out after ArtifactDays. A dry run only counts.
func Prune(ctx context.Context, in PruneInput) (*PruneReport, error) {
	if in.Logger == nil {
		in.Logger = slog.New(slog.DiscardHandler)
	}
	rep := &PruneReport{}
	outputDir := filepath.Join(in.DataDir, "output")
	entries, err := os.ReadDir(outputDir)
	if err != nil && !os.IsNotExist(err) {
		return nil, fmt.Errorf("read %s: %w", outputDir, err)
	}
	compressAfter := in.Now.AddDate(0, 0, -7)
	dropPlain := in.Now.AddDate(0, 0, -in.Retention.OutputDays)
	dropGz := in.Now.AddDate(0, 0, -in.Retention.TranscriptDays)
	for _, e := range entries {
		if e.IsDir() {
			continue
		}
		info, err := e.Info()
		if err != nil {
			return nil, fmt.Errorf("stat %s: %w", e.Name(), err)
		}
		path := filepath.Join(outputDir, e.Name())
		gz := strings.HasSuffix(e.Name(), ".gz")
		drop := dropPlain
		if gz {
			drop = dropGz
		}
		switch {
		case info.ModTime().Before(drop):
			rep.Outputs++
			rep.OutputBytes += info.Size()
			if in.Delete {
				if err := os.Remove(path); err != nil {
					return nil, fmt.Errorf("remove %s: %w", path, err)
				}
				in.Logger.InfoContext(ctx, "pruned output", "path", path, "bytes", info.Size())
			}
		case !gz && info.ModTime().Before(compressAfter):
			rep.Compressed++
			if in.Delete {
				if err := gzipInPlace(path, info.ModTime()); err != nil {
					return nil, err
				}
				in.Logger.InfoContext(ctx, "compressed transcript", "path", path)
			}
		}
	}
	artifactsDir := filepath.Join(in.DataDir, "artifacts")
	dirs, err := os.ReadDir(artifactsDir)
	if err != nil && !os.IsNotExist(err) {
		return nil, fmt.Errorf("read %s: %w", artifactsDir, err)
	}
	dropArtifacts := in.Now.AddDate(0, 0, -in.Retention.ArtifactDays)
	for _, d := range dirs {
		info, err := d.Info()
		if err != nil {
			return nil, fmt.Errorf("stat %s: %w", d.Name(), err)
		}
		if !info.ModTime().Before(dropArtifacts) {
			continue
		}
		rep.ArtifactDirs++
		if in.Delete {
			if err := os.RemoveAll(filepath.Join(artifactsDir, d.Name())); err != nil {
				return nil, fmt.Errorf("remove artifacts %s: %w", d.Name(), err)
			}
		}
	}
	return rep, nil
}

// gzipInPlace writes <path>.gz (preserving the mtime, which drives later
// retention) and removes the original only after a successful close.
func gzipInPlace(path string, mtime time.Time) (err error) {
	src, err := os.Open(path)
	if err != nil {
		return fmt.Errorf("open %s: %w", path, err)
	}
	defer func() { err = errors.Join(err, src.Close()) }()
	dst, err := os.OpenFile(path+".gz", os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
	if err != nil {
		return fmt.Errorf("create %s.gz: %w", path, err)
	}
	zw := gzip.NewWriter(dst)
	if _, err := io.Copy(zw, src); err != nil {
		return errors.Join(fmt.Errorf("compress %s: %w", path, err), zw.Close(), dst.Close())
	}
	if err := errors.Join(zw.Close(), dst.Close()); err != nil {
		return err
	}
	if err := os.Chtimes(path+".gz", mtime, mtime); err != nil {
		return fmt.Errorf("preserve mtime of %s.gz: %w", path, err)
	}
	if err := os.Remove(path); err != nil {
		return fmt.Errorf("remove %s after compressing: %w", path, err)
	}
	return nil
}
