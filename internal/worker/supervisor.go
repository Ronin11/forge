package worker

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
)

// Bounds on what one launch may hand back to the worker. Lines longer than
// MaxLineBytes are cut, stderr keeps only its tail, and the raw mirror stops
// growing at MaxOutput — so a runaway agent fills a bounded amount of memory
// and disk, never all of it.
const (
	MaxLineBytes = 1 << 20

	defaultMaxOutput = 64 << 20
	stderrTailBytes  = 64 << 10
	pumpBufferBytes  = 64 << 10
	drainGrace       = 5 * time.Second
	killGrace        = 5 * time.Second
	killPoll         = 25 * time.Millisecond
	truncatedMarker  = "\n[truncated]\n"
)

// errProcessChanged is returned when the pid a caller wants to signal is alive
// but has a different start time: it belongs to someone else now.
var errProcessChanged = errors.New("process identity changed")

// ProcessStart returns the start time (clock ticks since boot) of pid from
// /proc/<pid>/stat field 22 — the identity that makes pid reuse detectable.
func ProcessStart(pid int) (int64, error) {
	start, _, err := procStat(pid)
	return start, err
}

// procStat reads start time (field 22) and state (field 3) of pid. The comm
// field can contain spaces and parentheses, so fields are counted from the
// last ')' rather than from the start of the line.
func procStat(pid int) (start int64, state byte, err error) {
	if pid <= 0 {
		return 0, 0, fmt.Errorf("process %d: invalid pid", pid)
	}
	data, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/stat")
	if err != nil {
		return 0, 0, fmt.Errorf("process %d: %w", pid, err)
	}
	end := strings.LastIndexByte(string(data), ')')
	if end < 0 {
		return 0, 0, fmt.Errorf("process %d: malformed stat", pid)
	}
	fields := strings.Fields(string(data[end+1:]))
	// fields[0] is field 3 (state); field 22 is therefore fields[19].
	if len(fields) < 20 {
		return 0, 0, fmt.Errorf("process %d: malformed stat: %d fields after comm", pid, len(fields))
	}
	start, err = strconv.ParseInt(fields[19], 10, 64)
	if err != nil {
		return 0, 0, fmt.Errorf("process %d: parse start time: %w", pid, err)
	}
	return start, fields[0][0], nil
}

// ProcessAlive reports whether pid is running with the given start time. A
// pid that is gone, a zombie, or a different process is not alive; a pid we may
// not inspect (EPERM) is an error, because "alive with unknown identity" is
// exactly the case nothing should act on.
func ProcessAlive(pid int, start int64) (bool, error) {
	if err := syscall.Kill(pid, 0); err != nil {
		if errors.Is(err, syscall.ESRCH) {
			return false, nil
		}
		return false, fmt.Errorf("process %d: cannot verify identity: %w", pid, err)
	}
	got, state, err := procStat(pid)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return false, nil
		}
		return false, err
	}
	return got == start && state != 'Z', nil
}

// verifyLeader distinguishes the three states of a recorded pid: gone (nil —
// signalling the group is still safe because a live group pins its pgid),
// ours (nil), or someone else's (errProcessChanged, or an error when it cannot
// be inspected).
func verifyLeader(pid int, start int64) error {
	if err := syscall.Kill(pid, 0); err != nil {
		if errors.Is(err, syscall.ESRCH) {
			return nil
		}
		return fmt.Errorf("process %d: cannot verify identity: %w", pid, err)
	}
	got, _, err := procStat(pid)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil
		}
		return err
	}
	if got != start {
		return fmt.Errorf("process %d: start %d, recorded %d: %w", pid, got, start, errProcessChanged)
	}
	return nil
}

// signalGroup sends sig to every process in group pgid. ESRCH means the group
// is already gone, which is the outcome the caller wanted.
func signalGroup(pgid int, sig syscall.Signal) error {
	if err := syscall.Kill(-pgid, sig); err != nil && !errors.Is(err, syscall.ESRCH) {
		return fmt.Errorf("signal group %d with %s: %w", pgid, sig, err)
	}
	return nil
}

// groupAlive reports whether any member of pgid still exists. EPERM means a
// member exists that we may not signal, which for polling purposes is alive.
func groupAlive(pgid int) bool {
	err := syscall.Kill(-pgid, 0)
	return err == nil || errors.Is(err, syscall.EPERM)
}

// KillGroup sends SIGTERM to the process group pgid (= pid of a Setpgid child),
// polls every 25 ms up to grace, then SIGKILL. It re-verifies ProcessStart
// before each signal and refuses when the identity changed, so a recorded pid
// from a manifest can never be used to kill an unrelated process. ESRCH is
// success. grace 0 → TERM then immediate KILL, which sweeps stragglers after a
// natural exit.
func KillGroup(pid int, start int64, grace time.Duration) error {
	if err := verifyLeader(pid, start); err != nil {
		return fmt.Errorf("kill group %d: %w", pid, err)
	}
	if err := signalGroup(pid, syscall.SIGTERM); err != nil {
		return fmt.Errorf("kill group %d: %w", pid, err)
	}
	deadline := time.Now().Add(grace)
	for grace > 0 {
		if !groupAlive(pid) {
			return nil
		}
		if !time.Now().Before(deadline) {
			break
		}
		time.Sleep(killPoll)
	}
	if err := verifyLeader(pid, start); err != nil {
		return fmt.Errorf("kill group %d: %w", pid, err)
	}
	if err := signalGroup(pid, syscall.SIGKILL); err != nil {
		return fmt.Errorf("kill group %d: %w", pid, err)
	}
	return nil
}

// LaunchSpec is everything needed to run an executor once.
type LaunchSpec struct {
	Cmd        *exec.Cmd         // Path, Args, Dir, Env prepared by the executor; Stdin/Stdout/Stderr must be nil
	Prompt     string            // written to stdin, then stdin closed
	Timeout    time.Duration     // the attempt's remaining time; the group is killed at the deadline
	OutputPath string            // raw stdout+stderr mirror, created 0600; "" disables the mirror
	MaxOutput  int64             // bytes of raw mirror before truncation (default 64 MiB)
	OnStdout   func(line []byte) // called per stdout line (without newline); a line over MaxLineBytes is delivered cut to MaxLineBytes
	OnStderr   func(line []byte)
}

// ExitStatus is everything the attempt records about how the executor ended.
type ExitStatus struct {
	Code        int
	Signal      string
	TimedOut    bool
	Stopped     string // reason passed to Stop, "" otherwise
	OutputBytes int64
	Truncated   bool
	StderrTail  string
	Err         error
}

// Process is a running executor: the leader of its own process group, whose
// identity (pid + start time) was recorded before anything else could happen
// to it.
type Process struct {
	cmd   *exec.Cmd
	pid   int
	start int64

	stdinW         *os.File
	stdoutR        *os.File
	stderrR        *os.File
	mirror         *outputMirror
	stdoutCallback func([]byte)
	stderrCallback func([]byte)

	exited chan struct{} // closed once cmd.Wait has returned
	done   chan struct{} // closed once exit is final and every goroutine has finished

	// mu guards exit, exitedFlag, stopReason, and timedOut, which are written
	// by the waiter, Stop, and the deadline goroutine.
	mu         sync.Mutex
	exit       ExitStatus
	exitedFlag bool
	stopReason string
	timedOut   bool

	stderrTail []byte // owned by the stderr pump until it returns
	// pumps counts the four helpers (stdin writer, two pumps, deadline watcher);
	// the waiter is not in it, because the waiter is what joins them.
	pumps sync.WaitGroup
}

// Launch starts spec.Cmd in its own process group with the prompt on stdin, and
// returns once its pid identity is recorded. Pumps, the deadline, and the
// waiter are goroutines owned by the Process and joined by Wait.
func Launch(ctx context.Context, spec LaunchSpec) (p *Process, err error) {
	if err := checkLaunchSpec(spec); err != nil {
		return nil, fmt.Errorf("launch: %w", err)
	}
	maxOutput := spec.MaxOutput
	if maxOutput <= 0 {
		maxOutput = defaultMaxOutput
	}
	mirror, err := newOutputMirror(spec.OutputPath, maxOutput)
	if err != nil {
		return nil, fmt.Errorf("launch: %w", err)
	}
	stdinR, stdinW, stdoutR, stdoutW, stderrR, stderrW, err := launchPipes()
	if err != nil {
		return nil, errors.Join(fmt.Errorf("launch: %w", err), mirror.close())
	}
	cmd := spec.Cmd
	cmd.Stdin, cmd.Stdout, cmd.Stderr = stdinR, stdoutW, stderrW
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	p = &Process{
		cmd:            cmd,
		stdinW:         stdinW,
		stdoutR:        stdoutR,
		stderrR:        stderrR,
		mirror:         mirror,
		stdoutCallback: spec.OnStdout,
		stderrCallback: spec.OnStderr,
		exited:         make(chan struct{}),
		done:           make(chan struct{}),
	}
	startErr := cmd.Start()
	// The child's ends belong to the child now; the parent's copies would keep
	// the pipes open past its exit and the pumps would never see EOF.
	childEnds := errors.Join(stdinR.Close(), stdoutW.Close(), stderrW.Close())
	if startErr != nil {
		return nil, errors.Join(fmt.Errorf("launch: start %s: %w", cmd.Path, startErr), childEnds, p.closeParentEnds(), mirror.close())
	}
	if childEnds != nil {
		// The process is already running; reap it so the failure does not
		// leave an orphan, then report.
		_ = KillGroup(cmd.Process.Pid, 0, 0)
		_ = cmd.Wait()
		return nil, errors.Join(fmt.Errorf("launch: close child pipe ends: %w", childEnds), p.closeParentEnds(), mirror.close())
	}
	p.pid = cmd.Process.Pid
	p.start, err = ProcessStart(p.pid)
	if err != nil {
		// Identity is what lets everything later be verified; without it the
		// process is unusable and must not survive.
		_ = signalGroup(p.pid, syscall.SIGKILL)
		_ = cmd.Wait()
		return nil, errors.Join(fmt.Errorf("launch: record identity: %w", err), p.closeParentEnds(), mirror.close())
	}

	p.pumps.Add(4)
	go p.writeStdin(spec.Prompt)
	go p.pump(p.stdoutR, p.stdoutCallback, false)
	go p.pump(p.stderrR, p.stderrCallback, true)
	go p.watchDeadline(ctx, spec.Timeout)
	go p.wait()
	return p, nil
}

func checkLaunchSpec(spec LaunchSpec) error {
	switch {
	case spec.Cmd == nil:
		return errors.New("cmd is nil")
	case spec.Cmd.Process != nil:
		return errors.New("cmd was already started")
	case spec.Cmd.Stdin != nil || spec.Cmd.Stdout != nil || spec.Cmd.Stderr != nil:
		return errors.New("cmd stdin, stdout, and stderr must be nil")
	case spec.Cmd.Cancel != nil:
		// exec.CommandContext installs a Cancel that kills only the direct
		// child, leaving its process group running; Launch owns the deadline.
		return errors.New("cmd must come from exec.Command, not exec.CommandContext")
	case spec.Timeout <= 0:
		return fmt.Errorf("timeout %s is not positive", spec.Timeout)
	}
	return nil
}

func launchPipes() (stdinR, stdinW, stdoutR, stdoutW, stderrR, stderrW *os.File, err error) {
	if stdinR, stdinW, err = os.Pipe(); err != nil {
		return nil, nil, nil, nil, nil, nil, fmt.Errorf("stdin pipe: %w", err)
	}
	if stdoutR, stdoutW, err = os.Pipe(); err != nil {
		return nil, nil, nil, nil, nil, nil, errors.Join(fmt.Errorf("stdout pipe: %w", err), stdinR.Close(), stdinW.Close())
	}
	if stderrR, stderrW, err = os.Pipe(); err != nil {
		return nil, nil, nil, nil, nil, nil, errors.Join(fmt.Errorf("stderr pipe: %w", err), stdinR.Close(), stdinW.Close(), stdoutR.Close(), stdoutW.Close())
	}
	return stdinR, stdinW, stdoutR, stdoutW, stderrR, stderrW, nil
}

// closeParentEnds closes the pipe ends the parent reads and writes; closing
// wakes any pump blocked in Read, which is how the drain is bounded.
func (p *Process) closeParentEnds() error {
	return errors.Join(closeQuiet(p.stdinW), closeQuiet(p.stdoutR), closeQuiet(p.stderrR))
}

// closeQuiet treats a second Close as success, because the bounded drain and
// the normal path both close the same files.
func closeQuiet(f *os.File) error {
	if err := f.Close(); err != nil && !errors.Is(err, fs.ErrClosed) {
		return err
	}
	return nil
}

// PID is the process group leader's pid, which is also the pgid.
func (p *Process) PID() int { return p.pid }

// PIDStart is the leader's start time, recorded at launch for later
// verification.
func (p *Process) PIDStart() int64 { return p.start }

// writeStdin delivers the prompt and closes stdin so an executor reading to EOF
// starts. A child that exits without reading makes the write fail with EPIPE,
// which is not an error worth reporting: the exit status says what happened.
func (p *Process) writeStdin(prompt string) {
	defer p.pumps.Done()
	if _, err := io.WriteString(p.stdinW, prompt); err != nil && !errors.Is(err, syscall.EPIPE) && !errors.Is(err, fs.ErrClosed) {
		p.recordErr(fmt.Errorf("write prompt: %w", err))
	}
	if err := closeQuiet(p.stdinW); err != nil {
		p.recordErr(fmt.Errorf("close stdin: %w", err))
	}
}

// pump reads one stream line by line, honouring ReadLine's isPrefix so a line
// of any length is consumed without buffering more than MaxLineBytes of it, and
// mirrors every byte as it arrives.
func (p *Process) pump(r *os.File, callback func([]byte), isStderr bool) {
	defer p.pumps.Done()
	br := bufio.NewReaderSize(r, pumpBufferBytes)
	var line []byte
	for {
		fragment, isPrefix, err := br.ReadLine()
		if len(fragment) > 0 {
			p.mirror.write(fragment)
			if room := MaxLineBytes - len(line); room > 0 {
				line = append(line, fragment[:min(room, len(fragment))]...)
			}
		}
		if err != nil {
			if len(line) > 0 {
				p.deliver(line, callback, isStderr)
			}
			if !errors.Is(err, io.EOF) && !errors.Is(err, fs.ErrClosed) {
				p.recordErr(fmt.Errorf("read output: %w", err))
			}
			return
		}
		if isPrefix {
			continue
		}
		p.mirror.write([]byte{'\n'})
		p.deliver(line, callback, isStderr)
		line = line[:0]
	}
}

func (p *Process) deliver(line []byte, callback func([]byte), isStderr bool) {
	if isStderr {
		p.stderrTail = append(p.stderrTail, line...)
		p.stderrTail = append(p.stderrTail, '\n')
		if excess := len(p.stderrTail) - stderrTailBytes; excess > 0 {
			p.stderrTail = append(p.stderrTail[:0], p.stderrTail[excess:]...)
		}
	}
	if callback != nil {
		callback(line)
	}
}

// watchDeadline kills the group when the attempt's remaining time runs out or
// the launch context is cancelled. It is a goroutine rather than time.AfterFunc
// so that Wait can join it.
func (p *Process) watchDeadline(ctx context.Context, timeout time.Duration) {
	defer p.pumps.Done()
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case <-p.exited:
	case <-timer.C:
		p.mu.Lock()
		p.timedOut = true
		p.mu.Unlock()
		if err := KillGroup(p.pid, p.start, killGrace); err != nil {
			p.recordErr(fmt.Errorf("timeout: %w", err))
		}
	case <-ctx.Done():
		if err := p.Stop("context cancelled", killGrace); err != nil {
			p.recordErr(err)
		}
	}
}

// wait reaps the child, then gives the pumps a bounded time to drain: a
// grandchild holding the pipes open must not keep the attempt alive forever.
func (p *Process) wait() {
	waitErr := p.cmd.Wait()
	p.mu.Lock()
	p.exitedFlag = true
	p.mu.Unlock()
	close(p.exited)

	pumpsDone := make(chan struct{})
	go func() {
		// Owned by this goroutine: it returns as soon as the pumps do, and
		// the pumps are joined below in either branch.
		p.pumpsWait()
		close(pumpsDone)
	}()
	timer := time.NewTimer(drainGrace)
	defer timer.Stop()
	select {
	case <-pumpsDone:
	case <-timer.C:
		if err := errors.Join(closeQuiet(p.stdoutR), closeQuiet(p.stderrR)); err != nil {
			p.recordErr(fmt.Errorf("close pipes after drain grace: %w", err))
		}
		<-pumpsDone
	}
	p.finish(waitErr)
}

// pumpsWait blocks until the four helpers have returned.
func (p *Process) pumpsWait() {
	p.pumps.Wait()
}

func (p *Process) finish(waitErr error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.exit.Code, p.exit.Signal, p.exit.Err = exitStatusOf(waitErr, p.exit.Err)
	p.exit.TimedOut = p.timedOut
	p.exit.Stopped = p.stopReason
	p.exit.OutputBytes, p.exit.Truncated = p.mirror.stats()
	p.exit.StderrTail = string(p.stderrTail)
	p.exit.Err = errors.Join(p.exit.Err, errors.Join(p.closeParentEnds(), p.mirror.close()))
	close(p.done)
}

// exitStatusOf maps cmd.Wait's result to the shell convention: a normal exit
// keeps its code; a signal death is 128+signal with the signal named.
func exitStatusOf(waitErr, prior error) (code int, signal string, err error) {
	if waitErr == nil {
		return 0, "", prior
	}
	var exitErr *exec.ExitError
	if !errors.As(waitErr, &exitErr) {
		return -1, "", errors.Join(prior, fmt.Errorf("wait: %w", waitErr))
	}
	status, ok := exitErr.Sys().(syscall.WaitStatus)
	if !ok {
		return exitErr.ExitCode(), "", prior
	}
	if status.Signaled() {
		sig := status.Signal()
		return 128 + int(sig), signalName(sig), prior
	}
	return status.ExitStatus(), "", prior
}

func signalName(sig syscall.Signal) string {
	switch sig {
	case syscall.SIGHUP:
		return "SIGHUP"
	case syscall.SIGINT:
		return "SIGINT"
	case syscall.SIGKILL:
		return "SIGKILL"
	case syscall.SIGTERM:
		return "SIGTERM"
	case syscall.SIGSEGV:
		return "SIGSEGV"
	case syscall.SIGABRT:
		return "SIGABRT"
	case syscall.SIGPIPE:
		return "SIGPIPE"
	}
	return fmt.Sprintf("SIG%d", int(sig))
}

func (p *Process) recordErr(err error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.exit.Err = errors.Join(p.exit.Err, err)
}

// Wait blocks until the executor has exited and both pumps have drained — at
// most drainGrace after the exit, when the pipes are closed under them — and
// returns the final status. It may be called more than once.
func (p *Process) Wait() ExitStatus {
	<-p.done
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.exit
}

// Stop kills the group with the identity recorded at launch. The first reason
// wins and is reported as ExitStatus.Stopped; after the exit it is a no-op, so
// a cancel racing a natural exit is harmless.
func (p *Process) Stop(reason string, grace time.Duration) error {
	p.mu.Lock()
	if p.exitedFlag {
		p.mu.Unlock()
		return nil
	}
	if p.stopReason == "" {
		p.stopReason = reason
	}
	p.mu.Unlock()
	if err := KillGroup(p.pid, p.start, grace); err != nil {
		return fmt.Errorf("stop (%s): %w", reason, err)
	}
	return nil
}

// outputMirror is the bounded raw copy of stdout+stderr. Bytes past the bound
// are counted but not written, and one marker says so.
type outputMirror struct {
	file *os.File // nil when no mirror was requested
	max  int64

	// mu guards written, produced, truncated, and writes to file, which both
	// pumps share.
	mu        sync.Mutex
	written   int64
	produced  int64
	truncated bool
	err       error
}

func newOutputMirror(path string, max int64) (*outputMirror, error) {
	m := &outputMirror{max: max}
	if path == "" {
		return m, nil
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_TRUNC|syscall.O_NOFOLLOW, 0o600)
	if err != nil {
		return nil, fmt.Errorf("create output file: %w", err)
	}
	m.file = f
	return m, nil
}

func (m *outputMirror) write(b []byte) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.produced += int64(len(b))
	if m.file == nil || m.truncated {
		return
	}
	if room := m.max - m.written; int64(len(b)) > room {
		m.truncated = true
		b = append(append([]byte{}, b[:room]...), truncatedMarker...)
	}
	n, err := m.file.Write(b)
	m.written += int64(n)
	if err != nil && m.err == nil {
		m.err = fmt.Errorf("write output file: %w", err)
	}
}

func (m *outputMirror) stats() (produced int64, truncated bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.produced, m.truncated
}

func (m *outputMirror) close() error {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.file == nil {
		return m.err
	}
	err := errors.Join(m.err, closeQuiet(m.file))
	m.file = nil
	return err
}
