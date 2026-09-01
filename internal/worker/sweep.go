package worker

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"strconv"
	"syscall"
	"time"
)

// The environment markers every process an attempt spawns inherits. A process
// group catches the well-behaved descendants; one that left the group — a
// headless browser that calls setsid, a preview server a build backgrounded —
// is still recognisable by its environment, which is what Sweep matches on.
// AttemptEnv scopes a sweep to one attempt, WorkerEnv to everything this
// worker ever started (DESIGN.md §5.6, §7.2).
const (
	AttemptEnv = "FORGE_ATTEMPT"
	WorkerEnv  = "FORGE_WORKER"
)

// ProcessTags are the marker entries added to every environment Forge hands a
// child of an attempt, so the sweep can find its descendants later. They are
// deliberately absent from PassthroughEnv's allow-list: a marker is only ever
// set explicitly, never inherited from the worker's own environment.
func ProcessTags(workerID, attemptID string) []string {
	return []string{WorkerEnv + "=" + workerID, AttemptEnv + "=" + attemptID}
}

// Sweep kills every process whose environment carries key=value: SIGTERM to
// the process group of each one, a poll up to grace, then SIGKILL to whatever
// is left. It is the backstop behind KillGroup for descendants that escaped
// the attempt's process group, so nothing an attempt started outlives it.
//
// This process, pid 1, and this process's own process group are never
// signalled: when Forge runs under Forge the worker itself carries the outer
// attempt's markers. It returns how many tagged processes it found.
func Sweep(key, value string, grace time.Duration) (int, error) {
	marker := []byte(key + "=" + value)
	found, err := taggedProcesses(marker)
	if err != nil || len(found) == 0 {
		return 0, err
	}
	errs := signalProcesses(found, syscall.SIGTERM)
	deadline := time.Now().Add(grace)
	for time.Now().Before(deadline) {
		time.Sleep(killPoll)
		left, err := taggedProcesses(marker)
		if err != nil {
			return len(found), errors.Join(errs, err)
		}
		if len(left) == 0 {
			return len(found), errs
		}
	}
	left, err := taggedProcesses(marker)
	if err != nil {
		return len(found), errors.Join(errs, err)
	}
	if len(left) > 0 {
		errs = errors.Join(errs, signalProcesses(left, syscall.SIGKILL))
	}
	return len(found), errs
}

// taggedProcesses lists the live processes whose environment holds marker.
// A process we may not inspect, or one that exits mid-scan, is skipped: the
// scan is a best-effort snapshot by construction, which is why Sweep signals
// process groups rather than the pids it found.
func taggedProcesses(marker []byte) ([]int, error) {
	entries, err := os.ReadDir("/proc")
	if err != nil {
		return nil, fmt.Errorf("scan /proc: %w", err)
	}
	self, group := os.Getpid(), syscall.Getpgrp()
	var out []int
	for _, e := range entries {
		pid, err := strconv.Atoi(e.Name())
		if err != nil || pid <= 1 || pid == self {
			continue
		}
		env, err := os.ReadFile("/proc/" + e.Name() + "/environ")
		if err != nil || !hasEnviron(env, marker) {
			continue
		}
		if pgid, err := syscall.Getpgid(pid); err == nil && pgid == group {
			continue
		}
		out = append(out, pid)
	}
	return out, nil
}

// hasEnviron reports whether the NUL-separated environment block holds marker
// as a whole entry, so FORGE_ATTEMPT=<id> never matches a longer id.
func hasEnviron(env, marker []byte) bool {
	for _, entry := range bytes.Split(env, []byte{0}) {
		if bytes.Equal(entry, marker) {
			return true
		}
	}
	return false
}

// signalProcesses sends sig to the process group of every pid, each group once
// — the group is what catches the children a tagged process has forked since
// the scan. A pid whose group cannot be read, or whose group is one that must
// never be signalled, is signalled alone. ESRCH is the outcome we wanted.
func signalProcesses(pids []int, sig syscall.Signal) error {
	group := syscall.Getpgrp()
	seen := map[int]bool{}
	var errs error
	for _, pid := range pids {
		pgid, err := syscall.Getpgid(pid)
		if err == nil && pgid > 1 && pgid != group {
			if seen[pgid] {
				continue
			}
			seen[pgid] = true
			errs = errors.Join(errs, signalGroup(pgid, sig))
			continue
		}
		if err := syscall.Kill(pid, sig); err != nil && !errors.Is(err, syscall.ESRCH) {
			errs = errors.Join(errs, fmt.Errorf("signal process %d with %s: %w", pid, sig, err))
		}
	}
	return errs
}
