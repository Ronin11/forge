package worker

import (
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"forge/internal/core/logging"
)

func testSandbox(t *testing.T, writePaths ...string) *Sandbox {
	t.Helper()
	return &Sandbox{BwrapPath: "/usr/bin/bwrap", Home: "/home/u", DepsDir: "/home/u/.forge/deps", WritePaths: writePaths}
}

func testSpec() SandboxSpec {
	return SandboxSpec{
		Worktree:     "/data/worktrees/a1",
		ArtifactsDir: "/data/artifacts/a1",
		MCPConfig:    "/data/mcp/a1.json",
		Socket:       "/home/u/.forge/forge.sock",
		RepoGitDir:   "/home/u/Projects/demo/.git",
		ExecutorPath: "/usr/bin/claude",
		ForgeBin:     "/usr/bin/forge",
		ProxyURL:     "http://127.0.0.1:40001",
		Env:          []string{"PATH=/usr/bin", "HOME=/home/u", "SSH_AUTH_SOCK=/run/ssh", "ANTHROPIC_MODEL=x", "FORGE_HOME=/home/u/.forge", "GIT_CONFIG_COUNT=1", "USER=u", "TERM=xterm"},
	}
}

func TestSandboxArgsMountsExactlyTheAttempt(t *testing.T) {
	s := testSandbox(t, "/home/u/.claude", "/home/u/.claude.json")
	args := strings.Join(s.Args(testSpec()), " ")
	for _, want := range []string{
		"--die-with-parent", "--new-session", "--unshare-pid",
		"--proc /proc", "--dev /dev",
		"--ro-bind /usr /usr", "--ro-bind /etc /etc",
		"--tmpfs /tmp", "--tmpfs /run", "--tmpfs /home/u",
		"--ro-bind-try /home/u/.forge/deps /home/u/.forge/deps",
		"--bind /data/worktrees/a1 /data/worktrees/a1",
		"--bind /data/artifacts/a1 /data/artifacts/a1",
		"--ro-bind /data/mcp/a1.json /data/mcp/a1.json",
		"--bind-try /home/u/.forge/forge.sock /home/u/.forge/forge.sock",
		"--bind-try /home/u/Projects/demo/.git /home/u/Projects/demo/.git",
		"--bind-try /home/u/.claude /home/u/.claude",
		"--bind-try /home/u/.claude.json /home/u/.claude.json",
		"--chdir /data/worktrees/a1 --",
	} {
		if !strings.Contains(args, want) {
			t.Errorf("argv is missing %q:\n%s", want, args)
		}
	}
	// §19's never-exposed paths must not appear anywhere in the argv.
	for _, forbidden := range []string{".ssh", ".config/gh", "token"} {
		if strings.Contains(args, forbidden) {
			t.Errorf("argv mentions %q:\n%s", forbidden, args)
		}
	}
	// The tmpfs $HOME must be mounted before any bind inside it.
	tmpfsAt := strings.Index(args, "--tmpfs /home/u")
	sockAt := strings.Index(args, "--bind-try /home/u/.forge/forge.sock")
	if tmpfsAt < 0 || sockAt < tmpfsAt {
		t.Errorf("tmpfs home at %d must precede home binds at %d", tmpfsAt, sockAt)
	}
}

func TestSandboxWrapBuildsExplicitEnvironment(t *testing.T) {
	s := testSandbox(t)
	cmd := exec.Command("/usr/bin/claude", "--print")
	wrapped, err := s.Wrap(cmd, testSpec())
	if err != nil {
		t.Fatal(err)
	}
	if wrapped.Path != s.BwrapPath || wrapped.Args[0] != s.BwrapPath {
		t.Errorf("path = %s", wrapped.Path)
	}
	// The original argv follows the "--" separator verbatim.
	sep := slices.Index(wrapped.Args, "--")
	if sep < 0 || !slices.Equal(wrapped.Args[sep+1:], []string{"/usr/bin/claude", "--print"}) {
		t.Errorf("payload argv = %v", wrapped.Args[sep+1:])
	}
	env := strings.Join(wrapped.Env, "\n")
	for _, want := range []string{
		"PATH=/usr/bin", "HOME=/home/u", "TERM=xterm",
		"ANTHROPIC_MODEL=x", "FORGE_HOME=/home/u/.forge", "GIT_CONFIG_COUNT=1",
		"HTTP_PROXY=http://127.0.0.1:40001", "HTTPS_PROXY=http://127.0.0.1:40001", "NO_PROXY=",
	} {
		if !slices.ContainsFunc(wrapped.Env, func(kv string) bool { return kv == want }) {
			t.Errorf("env is missing %q:\n%s", want, env)
		}
	}
	for _, banned := range []string{"SSH_AUTH_SOCK", "USER="} {
		if strings.Contains(env, banned) {
			t.Errorf("env leaks %q:\n%s", banned, env)
		}
	}
	if len(cmd.Env) != 0 {
		t.Error("Wrap must not mutate the original command")
	}
}

func TestSandboxWrapRefusesBadSpecs(t *testing.T) {
	s := testSandbox(t)
	for name, mutate := range map[string]func(*SandboxSpec){
		"empty worktree":    func(sp *SandboxSpec) { sp.Worktree = "" },
		"relative mcp":      func(sp *SandboxSpec) { sp.MCPConfig = "mcp.json" },
		"empty executor":    func(sp *SandboxSpec) { sp.ExecutorPath = "" },
		"relative forgebin": func(sp *SandboxSpec) { sp.ForgeBin = "forge" },
	} {
		spec := testSpec()
		mutate(&spec)
		if _, err := s.Wrap(exec.Command("/bin/true"), spec); err == nil {
			t.Errorf("%s: no error", name)
		}
	}
}

// TestConfigRejectsForbiddenWritePaths: claude_write_paths become read-write
// mounts, so the never-exposed paths of §19 are refused at config load.
func TestConfigRejectsForbiddenWritePaths(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal(err)
	}
	for _, bad := range []string{"~/.ssh", "~/.ssh/id_ed25519", "~/.config/gh", filepath.Join(home, ".config", "gh", "hosts.yml")} {
		dir := t.TempDir()
		path := filepath.Join(dir, "worker.toml")
		body := "daemon = \"unix://" + dir + "/forge.sock\"\n" +
			"[executors.fake]\ncommand = [\"/bin/true\"]\noutput = \"lines\"\n" +
			"[sandbox]\nclaude_write_paths = [\"" + bad + "\"]\n"
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
		if _, err := LoadConfig(path); err == nil || !strings.Contains(err.Error(), "claude_write_paths") {
			t.Errorf("%s: want a claude_write_paths error, got %v", bad, err)
		}
	}
}

// TestConfigDefaultsWritePaths: an absent [sandbox] section gets the coarse
// placeholder defaults, expanded to absolute paths.
func TestConfigDefaultsWritePaths(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "worker.toml")
	body := "daemon = \"unix://" + dir + "/forge.sock\"\n[executors.fake]\ncommand = [\"/bin/true\"]\noutput = \"lines\"\n"
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal(err)
	}
	want := []string{filepath.Join(home, ".claude"), filepath.Join(home, ".claude.json")}
	if !slices.Equal(cfg.Sandbox.ClaudeWritePaths, want) {
		t.Errorf("write paths = %v, want %v", cfg.Sandbox.ClaudeWritePaths, want)
	}
}

// TestSandboxRealBwrap runs a real payload under the real bwrap argv and
// checks the §19 invariants from inside: ~/.ssh is invisible, $HOME is empty,
// /tmp is private, and the worktree is writable at its real path.
func TestSandboxRealBwrap(t *testing.T) {
	if _, err := exec.LookPath("bwrap"); err != nil {
		t.Skip("bwrap not installed")
	}
	forgeHome := t.TempDir()
	s := NewSandbox(forgeHome, SandboxSettings{ClaudeWritePaths: nil})
	if s == nil {
		t.Fatal("NewSandbox returned nil with bwrap present")
	}
	worktree := t.TempDir()
	artifacts := t.TempDir()
	mcp := filepath.Join(t.TempDir(), "mcp.json")
	if err := os.WriteFile(mcp, []byte("{}"), 0o600); err != nil {
		t.Fatal(err)
	}
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	script := `ls -d "$HOME/.ssh" 2>&1; echo "home=$(ls -A "$HOME" | tr '\n' ',')"; ` +
		`echo probe > /tmp/m8-tmp-probe; echo written > wt-probe.txt; pwd`
	wrapped, err := s.Wrap(exec.Command("/bin/sh", "-c", script), SandboxSpec{
		Worktree: worktree, ArtifactsDir: artifacts, MCPConfig: mcp,
		ExecutorPath: "/bin/sh", ForgeBin: self,
		Env: []string{"PATH=/usr/bin:/bin", "TERM=dumb"},
	})
	if err != nil {
		t.Fatal(err)
	}
	out, err := wrapped.CombinedOutput()
	if err != nil {
		t.Fatalf("bwrap run: %v\n%s", err, out)
	}
	text := string(out)
	if !strings.Contains(text, "No such file") {
		t.Errorf("~/.ssh should not exist inside the sandbox:\n%s", text)
	}
	if !strings.Contains(text, "home=\n") && !strings.Contains(text, "home=,") && !strings.Contains(text, "home=") {
		t.Errorf("no home listing:\n%s", text)
	}
	if strings.Contains(text, ".ssh,") || strings.Contains(text, ".config") {
		t.Errorf("tmpfs home leaks real home entries:\n%s", text)
	}
	if !strings.Contains(text, worktree) {
		t.Errorf("cwd should be the worktree at its real path:\n%s", text)
	}
	if data, err := os.ReadFile(filepath.Join(worktree, "wt-probe.txt")); err != nil || string(data) != "written\n" {
		t.Errorf("worktree write not visible outside: %q %v", data, err)
	}
	if _, err := os.Stat("/tmp/m8-tmp-probe"); !os.IsNotExist(err) {
		t.Errorf("/tmp is not private: %v", err)
	}
}

// TestSandboxRealBwrapProxy proves the topology decision: a sandboxed curl
// reaches the worker's loopback proxy via HTTP_PROXY, the allowlist admits the
// listed host, and an empty allowlist denies with a 403.
func TestSandboxRealBwrapProxy(t *testing.T) {
	if _, err := exec.LookPath("bwrap"); err != nil {
		t.Skip("bwrap not installed")
	}
	curl, err := exec.LookPath("curl")
	if err != nil {
		t.Skip("curl not installed")
	}
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if _, err := w.Write([]byte("proxied")); err != nil {
			t.Error(err)
		}
	}))
	defer ts.Close()
	run := func(allow []string) string {
		t.Helper()
		p, err := StartNetProxy(allow, nil, logging.Discard().For("test"))
		if err != nil {
			t.Fatal(err)
		}
		defer func() {
			if err := p.Close(); err != nil {
				t.Error(err)
			}
		}()
		s := NewSandbox(t.TempDir(), SandboxSettings{})
		mcp := filepath.Join(t.TempDir(), "mcp.json")
		if err := os.WriteFile(mcp, []byte("{}"), 0o600); err != nil {
			t.Fatal(err)
		}
		wrapped, err := s.Wrap(exec.Command(curl, "-s", "-S", "--max-time", "10", ts.URL), SandboxSpec{
			Worktree: t.TempDir(), ArtifactsDir: t.TempDir(), MCPConfig: mcp,
			ExecutorPath: curl, ForgeBin: curl, ProxyURL: p.URL(),
			Env: []string{"PATH=/usr/bin:/bin", "TERM=dumb"},
		})
		if err != nil {
			t.Fatal(err)
		}
		out, err := wrapped.CombinedOutput()
		if err != nil {
			t.Logf("curl exit: %v", err)
		}
		return string(out)
	}
	if out := run([]string{"127.0.0.1"}); out != "proxied" {
		t.Errorf("allowed fetch through sandboxed proxy = %q", out)
	}
	if out := run(nil); !strings.Contains(out, "not in the allowlist") {
		t.Errorf("denied fetch should carry the 403 body, got %q", out)
	}
}
