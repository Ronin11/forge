package worker

import (
	"errors"
	"fmt"
	"os"
	"strings"
	"syscall"
	"time"
)

// processIdentity returns a string that changes if pid is reused by another
// process: the start time from /proc/<pid>/stat (Linux only).
func processIdentity(pid int) (string, error) {
	body, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", pid))
	if err != nil {
		return "", err
	}
	// The comm field is parenthesised and may contain spaces; skip past it.
	text := string(body)
	end := strings.LastIndex(text, ")")
	if end < 0 {
		return "", errors.New("malformed /proc stat")
	}
	fields := strings.Fields(text[end+1:])
	// fields[0] is state (field 3); starttime is field 22 → index 19.
	if len(fields) < 20 {
		return "", errors.New("short /proc stat")
	}
	return fields[19], nil
}

// processGroupAlive reports whether any process remains in the group.
func processGroupAlive(pgid int) bool {
	err := syscall.Kill(-pgid, 0)
	return err == nil || errors.Is(err, syscall.EPERM)
}

// killProcessGroup sends SIGTERM to the group led by pid, waits up to grace,
// then SIGKILLs. It refuses to signal when pid no longer has the recorded
// identity (pid reuse). A dead group is not an error.
func killProcessGroup(pid int, identity string, grace time.Duration) error {
	current, err := processIdentity(pid)
	if err != nil {
		if !processGroupAlive(pid) {
			return nil
		}
		return fmt.Errorf("verify process %d: %w", pid, err)
	}
	if current != identity {
		return fmt.Errorf("refusing to signal pid %d: identity changed", pid)
	}
	if err := syscall.Kill(-pid, syscall.SIGTERM); err != nil && !errors.Is(err, syscall.ESRCH) {
		return fmt.Errorf("terminate process group %d: %w", pid, err)
	}
	deadline := time.Now().Add(grace)
	for time.Now().Before(deadline) {
		if !processGroupAlive(pid) {
			return nil
		}
		time.Sleep(25 * time.Millisecond)
	}
	if err := syscall.Kill(-pid, syscall.SIGKILL); err != nil && !errors.Is(err, syscall.ESRCH) {
		return fmt.Errorf("kill process group %d: %w", pid, err)
	}
	// Killed members linger as zombies until reaped; give them a moment.
	for deadline = time.Now().Add(time.Second); time.Now().Before(deadline) && processGroupAlive(pid); {
		time.Sleep(25 * time.Millisecond)
	}
	return nil
}
