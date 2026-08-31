package doctor

import (
	"fmt"
	"os"
	"path/filepath"
	"syscall"
)

// minFreeBytes is the disk headroom below which the home filesystem is a warn.
const minFreeBytes = 1 << 30 // 1 GiB

// binary is one required or recommended executable. gh and node (and npx,
// which ships with node) are recommended: Forge runs without them, degraded.
type binary struct {
	name     string
	required bool
}

var binaries = []binary{
	{"claude", true},
	{"git", true},
	{"gh", false},
	{"node", false},
	{"npx", false},
}

// Binaries checks the executables Forge shells out to. look is exec.LookPath;
// version reads a one-line version where that is cheap ("" when it is not, or
// on error) — both injected so tests never depend on the machine's PATH.
func Binaries(look func(name string) (path string, err error), version func(name string) string) []Check {
	out := make([]Check, 0, len(binaries))
	for _, b := range binaries {
		path, err := look(b.name)
		if err != nil {
			status, hint := StatusWarn, "install "+b.name+" to enable the features that use it"
			if b.required {
				status, hint = StatusFail, "install "+b.name+" and put it on PATH"
			}
			out = append(out, Check{Name: "binary." + b.name, Status: status, Detail: "not found on PATH", Hint: hint})
			continue
		}
		detail := path
		if v := version(b.name); v != "" {
			detail = v + " (" + path + ")"
		}
		out = append(out, Check{Name: "binary." + b.name, Status: StatusOK, Detail: detail})
	}
	return out
}

// Home checks that the Forge home exists and is private (0700): the socket,
// token, and database live under it.
func Home(home string) Check {
	fi, err := os.Stat(home)
	if os.IsNotExist(err) {
		return Check{Name: "home", Status: StatusFail, Detail: home + " does not exist", Hint: "run any forge command (or 'forge daemon start') to bootstrap it"}
	}
	if err != nil {
		return Check{Name: "home", Status: StatusFail, Detail: err.Error()}
	}
	if !fi.IsDir() {
		return Check{Name: "home", Status: StatusFail, Detail: home + " is not a directory"}
	}
	if perm := fi.Mode().Perm(); perm&0o077 != 0 {
		return Check{Name: "home", Status: StatusWarn, Detail: fmt.Sprintf("%s is %04o, want 0700", home, perm), Hint: "chmod 700 " + home}
	}
	return Check{Name: "home", Status: StatusOK, Detail: home}
}

// Token checks the worker token file: it authenticates TCP clients, so a
// group- or world-readable token is a hard failure.
func Token(path string) Check {
	fi, err := os.Stat(path)
	if os.IsNotExist(err) {
		return Check{Name: "token", Status: StatusWarn, Detail: path + " does not exist (daemon has not bootstrapped)", Hint: "start the daemon once to create it"}
	}
	if err != nil {
		return Check{Name: "token", Status: StatusFail, Detail: err.Error()}
	}
	if perm := fi.Mode().Perm(); perm&0o077 != 0 {
		return Check{Name: "token", Status: StatusFail, Detail: fmt.Sprintf("%s is %04o, want 0600", path, perm), Hint: "chmod 600 " + path}
	}
	return Check{Name: "token", Status: StatusOK, Detail: path}
}

// DB checks the SQLite file by stat only — doctor never opens the database
// (DESIGN.md §1.2), so it cannot contend with a live daemon.
func DB(path string) Check {
	fi, err := os.Stat(path)
	if os.IsNotExist(err) {
		return Check{Name: "db", Status: StatusWarn, Detail: path + " does not exist (daemon has not bootstrapped)"}
	}
	if err != nil {
		return Check{Name: "db", Status: StatusFail, Detail: err.Error()}
	}
	if perm := fi.Mode().Perm(); perm&0o077 != 0 {
		return Check{Name: "db", Status: StatusWarn, Detail: fmt.Sprintf("%s is %04o, want 0600", path, perm), Hint: "chmod 600 " + path}
	}
	return Check{Name: "db", Status: StatusOK, Detail: fmt.Sprintf("%s (%d bytes)", path, fi.Size())}
}

// Disk checks free space on the filesystem holding the home directory:
// worktrees and output files land there.
func Disk(home string) Check {
	var st syscall.Statfs_t
	if err := syscall.Statfs(home, &st); err != nil {
		return Check{Name: "disk", Status: StatusWarn, Detail: fmt.Sprintf("statfs %s: %v", home, err)}
	}
	free := uint64(st.Bavail) * uint64(st.Bsize)
	detail := fmt.Sprintf("%.1f GiB free on %s", float64(free)/(1<<30), filepath.Dir(home)+string(filepath.Separator)+filepath.Base(home))
	if free < minFreeBytes {
		return Check{Name: "disk", Status: StatusWarn, Detail: detail, Hint: "free disk space; attempts need room for worktrees and output"}
	}
	return Check{Name: "disk", Status: StatusOK, Detail: detail}
}

// DaemonFacts are the probed facts the socket and lock checks decide over. The
// caller gathers them with the one implementation of each probe (the flock
// helpers and daemon.json reader in controlplane), keeping those rules in
// their home while the decision lives here.
type DaemonFacts struct {
	SocketExists bool // <home>/forge.sock is present
	LockHeld     bool // a non-blocking flock probe on daemon.lock failed
	StateExists  bool // <home>/daemon.json is present
	PIDAlive     bool // daemon.json's pid is the process that wrote it (/proc)
	PID          int  // daemon.json's pid, 0 when StateExists is false
}

// Socket decides the socket-vs-liveness check: a socket file with nobody
// holding the lock is stale; a held lock with no socket is a daemon that lost
// its listener.
func Socket(f DaemonFacts) Check {
	switch {
	case f.LockHeld && f.SocketExists:
		detail := "daemon is running"
		if f.PIDAlive {
			detail = fmt.Sprintf("daemon pid %d is running", f.PID)
		}
		return Check{Name: "socket", Status: StatusOK, Detail: detail}
	case f.LockHeld && !f.SocketExists:
		return Check{Name: "socket", Status: StatusFail, Detail: "daemon holds the lock but forge.sock is missing", Hint: "forge daemon restart"}
	case !f.LockHeld && f.SocketExists:
		return Check{Name: "socket", Status: StatusWarn, Detail: "forge.sock exists but no daemon holds the lock (stale socket)", Hint: "forge daemon start (the daemon removes the stale socket)"}
	default:
		return Check{Name: "socket", Status: StatusOK, Detail: "daemon is not running"}
	}
}

// Lock decides the daemon.lock staleness check against daemon.json's pid.
func Lock(f DaemonFacts) Check {
	switch {
	case f.LockHeld && f.PIDAlive:
		return Check{Name: "lock", Status: StatusOK, Detail: fmt.Sprintf("held by pid %d", f.PID)}
	case f.LockHeld:
		return Check{Name: "lock", Status: StatusWarn, Detail: "daemon.lock is held but daemon.json names no live pid", Hint: "a daemon may be starting; if not, find the holder with 'fuser' and stop it"}
	case f.StateExists && !f.PIDAlive:
		return Check{Name: "lock", Status: StatusWarn, Detail: fmt.Sprintf("daemon.json names dead pid %d (unclean shutdown)", f.PID), Hint: "forge daemon start rewrites it"}
	default:
		return Check{Name: "lock", Status: StatusOK, Detail: "free"}
	}
}
