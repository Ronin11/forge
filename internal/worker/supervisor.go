package worker

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"syscall"
	"time"

	"forge/internal/protocol"
)

const (
	terminationGrace = 5 * time.Second
	maxOutputFile    = 64 << 20
	maxLineBytes     = 256 << 10
)

// RunSpec describes one executor launch.
type RunSpec struct {
	Argv       []string
	Dir        string
	Prompt     string
	Timeout    time.Duration
	Parser     Parser
	OutputFile string                               // raw stdout/stderr, bounded
	OnEvent    func(kind, message string)           // stdout summaries and stderr lines
	OnStart    func(pid int, identity string) error // called after start, before waiting
}

// RunOutcome is the supervisor's report.
type RunOutcome struct {
	ExitCode   int
	Reason     string // exited | timeout | cancelled
	Result     Result
	StderrTail string
	Err        error
}

// Run launches the executor in its own process group, feeds the prompt on
// stdin, streams stdout through the parser, and enforces the timeout. On
// timeout or ctx cancellation the whole process group is terminated.
func Run(ctx context.Context, spec RunSpec) RunOutcome {
	outcome := RunOutcome{Reason: "exited", ExitCode: -1}
	if len(spec.Argv) == 0 {
		outcome.Err = errors.New("empty executor command")
		return outcome
	}
	deadline, cancel := context.WithTimeout(ctx, spec.Timeout)
	defer cancel()

	cmd := exec.Command(spec.Argv[0], spec.Argv[1:]...)
	cmd.Dir = spec.Dir
	cmd.Stdin = strings.NewReader(spec.Prompt)
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		outcome.Err = err
		return outcome
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		outcome.Err = err
		return outcome
	}
	var raw *limitedFile
	if spec.OutputFile != "" {
		f, err := os.OpenFile(spec.OutputFile, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o600)
		if err != nil {
			outcome.Err = fmt.Errorf("open output file: %w", err)
			return outcome
		}
		raw = &limitedFile{f: f, limit: maxOutputFile}
		defer raw.Close()
	}
	if err := cmd.Start(); err != nil {
		outcome.Err = fmt.Errorf("start executor: %w", err)
		return outcome
	}
	pid := cmd.Process.Pid
	identity, idErr := processIdentity(pid)
	if spec.OnStart != nil {
		if err := spec.OnStart(pid, identity); err != nil {
			_ = syscall.Kill(-pid, syscall.SIGKILL)
			_ = cmd.Wait()
			outcome.Err = err
			return outcome
		}
	}

	tail := &tailBuffer{limit: protocol.MaxErrorBytes}
	var readers sync.WaitGroup
	readers.Add(2)
	go func() {
		defer readers.Done()
		readLines(stdout, func(line []byte) {
			raw.WriteLine("", line)
			if summary := spec.Parser.Line(line); summary != "" && spec.OnEvent != nil {
				spec.OnEvent("stdout", summary)
			}
		})
	}()
	go func() {
		defer readers.Done()
		readLines(stderr, func(line []byte) {
			raw.WriteLine("[stderr] ", line)
			_, _ = tail.Write(append(line, '\n'))
			if spec.OnEvent != nil {
				spec.OnEvent("stderr", boundedText(string(line), protocol.MaxEventMessage))
			}
		})
	}()

	waitErr := make(chan error, 1)
	go func() {
		readers.Wait() // Wait closes the pipes; drain them first.
		waitErr <- cmd.Wait()
	}()

	var runErr error
	select {
	case runErr = <-waitErr:
	case <-deadline.Done():
		if errors.Is(ctx.Err(), context.Canceled) {
			outcome.Reason = "cancelled"
		} else {
			outcome.Reason = "timeout"
		}
		if idErr == nil {
			if err := killProcessGroup(pid, identity, terminationGrace); err != nil {
				outcome.Err = err
			}
		} else {
			_ = syscall.Kill(-pid, syscall.SIGKILL)
		}
		select {
		case runErr = <-waitErr:
		case <-time.After(terminationGrace):
			_ = syscall.Kill(-pid, syscall.SIGKILL)
			runErr = <-waitErr
		}
	}
	outcome.ExitCode = exitCode(runErr)
	outcome.Result = spec.Parser.Result()
	outcome.StderrTail = tail.String()
	if outcome.Reason == "exited" && runErr != nil && outcome.Err == nil {
		outcome.Err = runErr
	}
	return outcome
}

// readLines calls fn for each line; lines beyond maxLineBytes are truncated.
func readLines(r io.Reader, fn func([]byte)) {
	br := bufio.NewReaderSize(r, 64<<10)
	for {
		line, err := br.ReadSlice('\n')
		if errors.Is(err, bufio.ErrBufferFull) {
			// Long line: keep the first chunk, drop the rest.
			keep := append([]byte(nil), line...)
			for errors.Is(err, bufio.ErrBufferFull) {
				var more []byte
				more, err = br.ReadSlice('\n')
				if len(keep) < maxLineBytes {
					keep = append(keep, more...)
				}
			}
			line = keep
		}
		if len(line) > 0 {
			fn(trimNewline(line))
		}
		if err != nil {
			return
		}
	}
}

func trimNewline(b []byte) []byte {
	for len(b) > 0 && (b[len(b)-1] == '\n' || b[len(b)-1] == '\r') {
		b = b[:len(b)-1]
	}
	return b
}

// limitedFile writes at most limit bytes then silently drops the rest.
type limitedFile struct {
	mu      sync.Mutex
	f       *os.File
	written int
	limit   int
}

func (l *limitedFile) WriteLine(prefix string, line []byte) {
	if l == nil {
		return
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	n := len(prefix) + len(line) + 1
	if l.written+n > l.limit {
		return
	}
	l.written += n
	_, _ = l.f.WriteString(prefix)
	_, _ = l.f.Write(line)
	_, _ = l.f.Write([]byte{'\n'})
}

func (l *limitedFile) Close() {
	if l == nil {
		return
	}
	_ = l.f.Close()
}
