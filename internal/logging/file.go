package logging

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"sync"
)

// FileOptions configure the always-on JSON file sink.
type FileOptions struct {
	Dir      string // directory; the file is <Dir>/<name>.log
	MaxBytes int64  // rotate when the file would exceed this
	MaxFiles int    // rotated generations kept (<name>.log.1 … .N)
}

// FileSink is a size-rotating log file. It is deliberately small: append, check
// size, rename the generations, reopen. No time-based rotation, no compression —
// `forge prune` owns retention.
type FileSink struct {
	path string
	opts FileOptions

	mu   sync.Mutex // guards file and size
	file *os.File
	size int64
}

// OpenFileSink opens (or creates, 0600) <opts.Dir>/<name>.log. name is the
// process component (daemon, worker). It takes no context: its only I/O is a
// handful of non-blocking syscalls (STYLE.md §3 carve-out).
func OpenFileSink(name string, opts FileOptions) (*FileSink, error) {
	if err := os.MkdirAll(opts.Dir, 0o700); err != nil {
		return nil, fmt.Errorf("create log directory %s: %w", opts.Dir, err)
	}
	s := &FileSink{path: filepath.Join(opts.Dir, name+".log"), opts: opts}
	if err := s.open(); err != nil {
		return nil, err
	}
	return s, nil
}

func (s *FileSink) open() error {
	f, err := os.OpenFile(s.path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return fmt.Errorf("open log file %s: %w", s.path, err)
	}
	info, err := f.Stat()
	if err != nil {
		return fmt.Errorf("stat log file %s: %w", s.path, errors.Join(err, f.Close()))
	}
	s.file, s.size = f, info.Size()
	return nil
}

// Write appends one record, rotating first if the record would push the file
// past MaxBytes. A single record is never split across files.
func (s *FileSink) Write(p []byte) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.size > 0 && s.size+int64(len(p)) > s.opts.MaxBytes {
		if err := s.rotate(); err != nil {
			return 0, err
		}
	}
	n, err := s.file.Write(p)
	s.size += int64(n)
	return n, err
}

// rotate shifts <path>.N-1 → <path>.N for N down to 1, then <path> → <path>.1,
// dropping the oldest, then opens a fresh live file and only then closes the old
// descriptor. If any step fails the old file stays open and writable, so a
// rotation problem costs an oversized log, never a wedged sink. Called with mu
// held.
func (s *FileSink) rotate() error {
	oldest := s.path + "." + strconv.Itoa(s.opts.MaxFiles)
	if err := os.Remove(oldest); err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("remove oldest log %s: %w", oldest, err)
	}
	for n := s.opts.MaxFiles - 1; n >= 1; n-- {
		from := s.path + "." + strconv.Itoa(n)
		to := s.path + "." + strconv.Itoa(n+1)
		if err := os.Rename(from, to); err != nil && !os.IsNotExist(err) {
			return fmt.Errorf("rotate %s: %w", from, err)
		}
	}
	if s.opts.MaxFiles >= 1 {
		if err := os.Rename(s.path, s.path+".1"); err != nil && !os.IsNotExist(err) {
			return fmt.Errorf("rotate %s: %w", s.path, err)
		}
	} else if err := os.Remove(s.path); err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("truncate %s: %w", s.path, err)
	}
	old := s.file
	if err := s.open(); err != nil {
		s.file = old // keep writing to the renamed file rather than nothing
		return err
	}
	return old.Close()
}

// Close releases the descriptor; writes are unbuffered so nothing is lost.
func (s *FileSink) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.file.Close()
}

// Path is where the sink writes; reported at startup so an operator can find it.
func (s *FileSink) Path() string { return s.path }
