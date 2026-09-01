package worker

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// Sandbox wraps an executor launch in bubblewrap (DESIGN.md §19). It is one
// pure argv transformation applied at the launch site, so the supervisor's
// process-group and timeout handling is unchanged: bwrap is the group leader,
// --die-with-parent delivers SIGKILL to the payload when the group is killed,
// and --unshare-pid tears the inner pid namespace down with it.
//
// Network: the sandbox deliberately does NOT --unshare-net. The per-attempt
// egress proxy (NetProxy) listens on the worker's loopback and is reached via
// HTTP_PROXY/HTTPS_PROXY; with an unshared network namespace the payload could
// not reach it without a unix-socket forwarder or slirp/pasta inside, neither
// of which is available here. Every proxy-respecting client (claude, curl,
// npm, go) therefore goes through the allowlist; a process that direct-dials
// sockets bypasses it. Recorded as an M8 known limitation.
type Sandbox struct {
	// BwrapPath is the absolute bwrap binary.
	BwrapPath string
	// Home is the user's real home directory: a tmpfs inside the sandbox,
	// with WritePaths and the attempt's own paths as the only holes.
	Home string
	// DepsDir is <forge home>/deps, mounted read-only when present — the one
	// part of the Forge home an executor may see (DESIGN §19).
	DepsDir string
	// WritePaths are executor state paths bind-mounted read-write
	// ([sandbox] claude_write_paths in worker.toml).
	WritePaths []string
}

// NewSandbox returns the wrapper, or nil when bwrap (or a resolvable home) is
// missing — the same condition the worker advertises as `sandbox: missing`.
func NewSandbox(forgeHome string, s SandboxSettings) *Sandbox {
	bwrap, err := exec.LookPath("bwrap")
	if err != nil {
		return nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return nil
	}
	return &Sandbox{BwrapPath: bwrap, Home: home, DepsDir: filepath.Join(forgeHome, "deps"), WritePaths: s.ClaudeWritePaths}
}

// SandboxSpec is what one attempt exposes to its sandboxed executor. Paths are
// absolute; optional ones may be empty or absent on disk and are then skipped
// (bwrap's -try mounts).
type SandboxSpec struct {
	Worktree     string // read-write, at its real path; also the cwd
	ArtifactsDir string // read-write (FORGE_ARTIFACTS)
	MCPConfig    string // the per-attempt MCP config file, read-only
	// Socket is the unix socket the MCP config's FORGE_SOCKET names. Today
	// that is the daemon socket (writeMCPConfig); §19's worker-side
	// per-attempt forwarding socket replaces it when it lands. Empty for an
	// http:// daemon.
	Socket string
	// RepoGitDir is the registered checkout's .git directory, read-write:
	// the worktree's gitdir and object store live there, so git inside the
	// sandbox cannot commit without it. Empty for greenfield attempts.
	RepoGitDir string
	// ExecutorPath is the resolved executor binary, read-only (it may live
	// under $HOME, which is otherwise a tmpfs).
	ExecutorPath string
	// ForgeBin is this forge binary, read-only: `forge mcp` runs inside.
	ForgeBin string
	// ExtraRO are additional read-only mounts (the fake executor's fixture
	// directory in tests and smoke).
	ExtraRO []string
	// ProxyURL is the NetProxy address for HTTP_PROXY/HTTPS_PROXY.
	ProxyURL string
	// Env is the attempt's already-filtered environment (attempt.env);
	// Wrap narrows it further and builds the final environment explicitly.
	Env []string
}

// Wrap returns a new command that runs cmd's argv under bwrap with the §19
// mounts and a minimal explicit environment. cmd itself is not mutated.
func (s *Sandbox) Wrap(cmd *exec.Cmd, spec SandboxSpec) (*exec.Cmd, error) {
	for name, p := range map[string]string{"worktree": spec.Worktree, "artifacts dir": spec.ArtifactsDir, "mcp config": spec.MCPConfig, "executor": spec.ExecutorPath, "forge binary": spec.ForgeBin} {
		if p == "" {
			return nil, fmt.Errorf("sandbox: %s path is empty", name)
		}
		if !filepath.IsAbs(p) {
			return nil, fmt.Errorf("sandbox: %s path %q is not absolute", name, p)
		}
	}
	args := s.Args(spec)
	args = append(args, cmd.Args...)
	wrapped := exec.Command(s.BwrapPath, args...)
	wrapped.Dir = spec.Worktree
	wrapped.Env = s.env(spec)
	return wrapped, nil
}

// Args is the bwrap argv up to and including the "--" separator, exactly per
// DESIGN.md §19: /usr, /etc, /lib*, /bin, /sbin read-only; private /proc,
// /dev, /tmp, /run; tmpfs $HOME with only the attempt's holes; --unshare-pid,
// --die-with-parent, --new-session. Nothing else from the host is visible —
// in particular ~/.ssh, ~/.config/gh, the Forge home (but for deps), the
// worker token, and other checkouts are absent, not merely masked.
func (s *Sandbox) Args(spec SandboxSpec) []string {
	args := []string{
		"--die-with-parent", "--new-session", "--unshare-pid",
		"--proc", "/proc", "--dev", "/dev",
		"--ro-bind", "/usr", "/usr",
		"--ro-bind", "/etc", "/etc",
	}
	args = append(args, rootLinks()...)
	args = append(args,
		"--tmpfs", "/tmp",
		"--tmpfs", "/run",
		"--tmpfs", s.Home,
	)
	// Order matters: everything under $HOME must be bound after its tmpfs.
	args = append(args, "--ro-bind-try", s.DepsDir, s.DepsDir)
	args = append(args, "--bind", spec.Worktree, spec.Worktree)
	args = append(args, "--bind", spec.ArtifactsDir, spec.ArtifactsDir)
	args = append(args, "--ro-bind", spec.MCPConfig, spec.MCPConfig)
	if spec.Socket != "" {
		args = append(args, "--bind-try", spec.Socket, spec.Socket)
	}
	if spec.RepoGitDir != "" {
		args = append(args, "--bind-try", spec.RepoGitDir, spec.RepoGitDir)
	}
	for _, p := range binaryPaths(spec.ExecutorPath, spec.ForgeBin) {
		args = append(args, "--ro-bind-try", p, p)
	}
	for _, p := range s.WritePaths {
		args = append(args, "--bind-try", p, p)
	}
	for _, p := range spec.ExtraRO {
		args = append(args, "--ro-bind-try", p, p)
	}
	args = append(args, "--chdir", spec.Worktree, "--")
	return args
}

// env is the explicit sandbox environment (§19): PATH, HOME, LANG, TERM, the
// executor's own variables (ANTHROPIC_*, CLAUDE_CONFIG_DIR), the FORGE_* the
// MCP config and artifacts contract need, GIT_CONFIG_* from the claim policy,
// and the proxy variables. Built from the attempt's env by allow-list; nothing
// else is inherited. NO_PROXY is set empty so no inherited exclusion can route
// around the proxy.
func (s *Sandbox) env(spec SandboxSpec) []string {
	keep := func(key string) bool {
		switch key {
		case "PATH", "LANG", "TERM", "CLAUDE_CONFIG_DIR":
			return true
		case "HOME", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy":
			return false // set explicitly below
		}
		for _, prefix := range []string{"LC_", "ANTHROPIC_", "FORGE_", "GIT_CONFIG_"} {
			if strings.HasPrefix(key, prefix) {
				return true
			}
		}
		return false
	}
	env := make([]string, 0, len(spec.Env)+6)
	for _, kv := range spec.Env {
		if key, _, ok := strings.Cut(kv, "="); ok && keep(key) {
			env = append(env, kv)
		}
	}
	env = append(env, "HOME="+s.Home)
	if spec.ProxyURL != "" {
		env = append(env,
			"HTTP_PROXY="+spec.ProxyURL, "HTTPS_PROXY="+spec.ProxyURL, "NO_PROXY=",
			"http_proxy="+spec.ProxyURL, "https_proxy="+spec.ProxyURL, "no_proxy=")
	}
	return env
}

// rootLinks reproduces the usr-merge symlinks (/bin → usr/bin and friends) so
// interpreters and shebangs resolve; a distribution where one of them is a
// real directory gets it read-only instead.
func rootLinks() []string {
	var args []string
	for _, p := range []string{"/bin", "/sbin", "/lib", "/lib64", "/lib32"} {
		fi, err := os.Lstat(p)
		if err != nil {
			continue
		}
		if fi.Mode()&os.ModeSymlink != 0 {
			if target, err := os.Readlink(p); err == nil {
				args = append(args, "--symlink", target, p)
			}
			continue
		}
		if fi.IsDir() {
			args = append(args, "--ro-bind", p, p)
		}
	}
	return args
}

// binaryPaths lists a binary and its symlink resolution (deduplicated): a
// launcher under $HOME would otherwise vanish into the tmpfs, and a symlinked
// one would dangle.
func binaryPaths(bins ...string) []string {
	seen := map[string]bool{}
	var out []string
	add := func(p string) {
		if p != "" && !seen[p] {
			seen[p] = true
			out = append(out, p)
		}
	}
	for _, b := range bins {
		add(b)
		if resolved, err := filepath.EvalSymlinks(b); err == nil {
			add(resolved)
		}
	}
	return out
}
