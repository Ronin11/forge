package worker

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"maps"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"

	"forge/internal/protocol"
)

// gitOutputLimit bounds what one git command may hand back: large enough for any
// porcelain listing Forge parses, small enough that a runaway command cannot
// exhaust the worker's memory.
const gitOutputLimit = 1 << 20

// gitErrorTail is how much of a failed command's stderr an error message carries;
// git's diagnostics fit in far less, and the rest would only bloat logs.
const gitErrorTail = 2048

// gitDefaultTimeout caps every git command so a hung credential helper or an
// unreachable remote cannot stall an attempt indefinitely (DESIGN §5.2).
const gitDefaultTimeout = 30 * time.Second

// gitWaitDelay bounds how long Run waits for output pipes after the process group
// has been killed, in case a helper escaped the group (setsid) and still holds them.
const gitWaitDelay = 5 * time.Second

// Git runs git with a per-command timeout, its own process group, and bounded
// output. It is a value, not a service: it holds no state beyond the timeout, so a
// zero Git is ready to use and tests can shorten the timeout freely.
type Git struct {
	// Timeout caps each command; zero means gitDefaultTimeout.
	Timeout time.Duration
	// Env is appended to the inherited environment of every command — the
	// integrator's GIT_CONFIG_* options travel here (STYLE.md §10: never a
	// .git/config write).
	Env []string
}

// Repository is a validated registered checkout. Path is canonical so every later
// comparison (manifests, worktree listings, duplicate detection) is string
// equality, and OriginIdentity is pinned at validation so a swapped remote is
// detected rather than fetched from.
type Repository struct {
	Name           string
	Path           string // canonical (EvalSymlinks)
	OriginURL      string
	OriginIdentity string // normalised, see NormalizeRemoteIdentity
	BaseBranch     string // configured, may be ""
	Project        string // advertised on registration; "" means default
}

// WorktreeState is what the filesystem and git's registry say about one worktree
// path. Cleanup decisions need both views because they can disagree after a crash.
type WorktreeState struct {
	PathExists, Registered bool
	Branch, Head           string
}

// limitBuffer keeps the first limit bytes written and remembers that more arrived,
// so a chatty child cannot grow memory without bound.
type limitBuffer struct {
	limit     int
	buf       bytes.Buffer
	truncated bool
}

func (b *limitBuffer) Write(p []byte) (int, error) {
	n := len(p)
	room := b.limit - b.buf.Len()
	if room <= 0 {
		if n > 0 {
			b.truncated = true
		}
		return n, nil
	}
	if len(p) > room {
		b.truncated = true
		p = p[:room]
	}
	b.buf.Write(p)
	return n, nil
}

// Run executes git in dir and returns its stdout. The child gets its own process
// group and the whole group is killed when the per-command timeout or ctx
// expires, so helpers git spawned (ssh, credential helpers) cannot outlive it.
// Errors read "git <args[0]>: <err>: <stderr or stdout tail>".
func (g Git) Run(ctx context.Context, dir string, args ...string) (string, error) {
	if len(args) == 0 {
		return "", errors.New("git: no arguments")
	}
	timeout := g.Timeout
	if timeout <= 0 {
		timeout = gitDefaultTimeout
	}
	runCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	cmd := exec.CommandContext(runCtx, "git", args...)
	cmd.Dir = dir
	// GIT_TERMINAL_PROMPT=0 turns a missing credential into an error instead of a
	// prompt nobody will answer. The user's global config must still apply, so
	// nothing else is overridden.
	cmd.Env = append(append(os.Environ(), g.Env...), "GIT_TERMINAL_PROMPT=0")
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	cmd.Cancel = func() error { return killProcessGroup(cmd.Process.Pid) }
	cmd.WaitDelay = gitWaitDelay
	stdout := &limitBuffer{limit: gitOutputLimit}
	stderr := &limitBuffer{limit: gitOutputLimit}
	cmd.Stdout, cmd.Stderr = stdout, stderr

	err := cmd.Run()
	if err != nil {
		switch {
		case ctx.Err() != nil:
			err = ctx.Err()
		case runCtx.Err() != nil:
			err = fmt.Errorf("timed out after %s: %w", timeout, runCtx.Err())
		}
		if tail := outputTail(stderr, stdout); tail != "" {
			return stdout.buf.String(), fmt.Errorf("git %s: %w: %s", args[0], err, tail)
		}
		return stdout.buf.String(), fmt.Errorf("git %s: %w", args[0], err)
	}
	// Truncated stdout would be silently misparsed by callers; failing is safer.
	if stdout.truncated {
		return "", fmt.Errorf("git %s: stdout exceeded %d bytes", args[0], gitOutputLimit)
	}
	return stdout.buf.String(), nil
}

// killProcessGroup sends SIGKILL to pid's whole process group. ESRCH is reported
// as os.ErrProcessDone so os/exec treats an already-exited child as success.
func killProcessGroup(pid int) error {
	if err := syscall.Kill(-pid, syscall.SIGKILL); err != nil {
		if errors.Is(err, syscall.ESRCH) {
			return os.ErrProcessDone
		}
		return fmt.Errorf("kill process group %d: %w", pid, err)
	}
	return nil
}

// outputTail picks the text an error should carry: stderr when git wrote any,
// otherwise stdout, trimmed to the last gitErrorTail bytes.
func outputTail(stderr, stdout *limitBuffer) string {
	s := strings.TrimSpace(stderr.buf.String())
	if s == "" {
		s = strings.TrimSpace(stdout.buf.String())
	}
	if len(s) > gitErrorTail {
		s = s[len(s)-gitErrorTail:]
	}
	return s
}

// isFullCommit accepts only complete SHA-1 or SHA-256 object names; abbreviated
// IDs are ambiguous over time and never stored.
func isFullCommit(s string) bool {
	if len(s) != 40 && len(s) != 64 {
		return false
	}
	for _, c := range s {
		if (c < '0' || c > '9') && (c < 'a' || c > 'f') {
			return false
		}
	}
	return true
}

// ValidateRepository turns a configured name/path pair into a Repository, applying
// DESIGN §7.1: canonical path, a non-bare worktree, a mandatory origin, and a
// well-formed base_branch. Failing here at start is cheaper than failing inside
// an attempt, so every check is fatal.
func (g Git) ValidateRepository(ctx context.Context, name, path, baseBranch string) (*Repository, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return nil, fmt.Errorf("repository %s: resolve path %s: %w", name, path, err)
	}
	canonical, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return nil, fmt.Errorf("repository %s: canonicalize path %s: %w", name, abs, err)
	}
	info, err := os.Stat(canonical)
	if err != nil {
		return nil, fmt.Errorf("repository %s: inspect path %s: %w", name, canonical, err)
	}
	if !info.IsDir() {
		return nil, fmt.Errorf("repository %s: %s is not a directory", name, canonical)
	}
	out, err := g.Run(ctx, canonical, "rev-parse", "--is-inside-work-tree")
	if err != nil {
		return nil, fmt.Errorf("repository %s: verify Git worktree at %s: %w", name, canonical, err)
	}
	if strings.TrimSpace(out) != "true" {
		return nil, fmt.Errorf("repository %s: %s is not a Git worktree (bare repositories are not supported)", name, canonical)
	}
	out, err = g.Run(ctx, canonical, "remote", "get-url", "origin")
	if err != nil {
		return nil, fmt.Errorf("repository %s: read origin remote: %w", name, err)
	}
	originURL := strings.TrimSpace(out)
	if originURL == "" {
		return nil, fmt.Errorf("repository %s: origin remote is required", name)
	}
	identity, err := NormalizeRemoteIdentity(originURL, canonical)
	if err != nil {
		return nil, fmt.Errorf("repository %s: normalize origin %q: %w", name, originURL, err)
	}
	if baseBranch != "" {
		if err := g.validateBaseBranch(ctx, canonical, baseBranch); err != nil {
			return nil, fmt.Errorf("repository %s: validate base_branch: %w", name, err)
		}
	}
	return &Repository{
		Name:           name,
		Path:           canonical,
		OriginURL:      originURL,
		OriginIdentity: identity,
		BaseBranch:     baseBranch,
	}, nil
}

// validateBaseBranch insists on a short branch name: base_branch feeds into
// refspecs and "origin/<b>" ref names, so HEAD, refs/… and origin/… would all
// resolve to something other than what the operator meant.
func (g Git) validateBaseBranch(ctx context.Context, dir, branch string) error {
	if branch == "" {
		return errors.New("base branch is empty")
	}
	if branch == "HEAD" || strings.HasPrefix(branch, "refs/") || strings.HasPrefix(branch, "origin/") || strings.HasPrefix(branch, "-") {
		return fmt.Errorf("base branch %q must be a short branch name such as main or release/2026.07", branch)
	}
	if _, err := g.Run(ctx, dir, "check-ref-format", "refs/heads/"+branch); err != nil {
		return fmt.Errorf("base branch %q is not a valid branch name: %w", branch, err)
	}
	return nil
}

// NormalizeRemoteIdentity reduces the many spellings of one remote to a single
// comparable string: scp-like user@host:path and URLs become host/path (host
// lowercased, path case preserved, leading "/", trailing "/" and ".git" removed);
// file:// and bare paths become "file://" plus the canonical path, relative paths
// resolved against repoPath. The identity is pinned at registration and compared
// on every fetch, so it must not depend on how the remote happened to be typed.
func NormalizeRemoteIdentity(remote, repoPath string) (string, error) {
	remote = strings.TrimSpace(remote)
	if remote == "" {
		return "", errors.New("origin remote is empty")
	}
	if strings.Contains(remote, "://") {
		u, err := url.Parse(remote)
		if err != nil {
			return "", fmt.Errorf("parse origin remote URL: %w", err)
		}
		if u.Scheme == "file" {
			return fileIdentity(u.Path, repoPath)
		}
		host := strings.ToLower(u.Host)
		path := trimIdentityPath(u.Path)
		if host == "" || path == "" {
			return "", errors.New("origin remote URL is malformed")
		}
		return host + "/" + path, nil
	}
	// scp-like: a colon before any slash, as git itself decides.
	if colon := strings.Index(remote, ":"); colon > 0 && !strings.Contains(remote[:colon], "/") {
		host := remote[:colon]
		if at := strings.LastIndex(host, "@"); at >= 0 {
			host = host[at+1:]
		}
		host = strings.ToLower(host)
		path := trimIdentityPath(remote[colon+1:])
		if host == "" || path == "" {
			return "", errors.New("origin SSH remote is malformed")
		}
		return host + "/" + path, nil
	}
	return fileIdentity(remote, repoPath)
}

// fileIdentity canonicalises a local remote so two spellings of one directory
// (relative, symlinked) compare equal.
func fileIdentity(path, repoPath string) (string, error) {
	if path == "" {
		return "", errors.New("origin file remote has no path")
	}
	if !filepath.IsAbs(path) {
		path = filepath.Join(repoPath, path)
	}
	canonical, err := filepath.EvalSymlinks(path)
	if err != nil {
		return "", fmt.Errorf("canonicalize origin path %s: %w", path, err)
	}
	return "file://" + canonical, nil
}

// trimIdentityPath strips the parts of a remote path that vary without changing
// which repository is meant.
func trimIdentityPath(path string) string {
	path = strings.Trim(path, "/")
	path = strings.TrimSuffix(path, ".git")
	return strings.TrimSuffix(path, "/")
}

// SameIdentity compares two normalised identities. GitHub slugs are
// case-insensitive, so github.com identities compare that way; every other host
// is compared exactly because case sensitivity there is unknown.
func SameIdentity(a, b string) bool {
	if strings.HasPrefix(a, "github.com/") && strings.HasPrefix(b, "github.com/") {
		return strings.EqualFold(a, b)
	}
	return a == b
}

// CheckOrigin re-reads origin and compares it with the pinned identity. It runs
// before and after a fetch (Factory's TOCTOU guard) so a remote swapped while the
// worker runs is refused rather than silently fetched from.
func (g Git) CheckOrigin(ctx context.Context, r *Repository) error {
	out, err := g.Run(ctx, r.Path, "remote", "get-url", "origin")
	if err != nil {
		return fmt.Errorf("check origin of %s: %w", r.Name, err)
	}
	identity, err := NormalizeRemoteIdentity(strings.TrimSpace(out), r.Path)
	if err != nil {
		return fmt.Errorf("check origin of %s: %w", r.Name, err)
	}
	if !SameIdentity(identity, r.OriginIdentity) {
		return fmt.Errorf("check origin of %s: origin changed since registration (%s, now %s)", r.Name, r.OriginIdentity, identity)
	}
	return nil
}

// Fetch updates refs/remotes/origin/<base> from origin, touching nothing else in
// the checkout. It is best-effort (DESIGN §5.2): a failed fetch is only fatal
// when there is no local remote-tracking ref to fall back on; otherwise the
// failure is returned as warn and the attempt proceeds on the stale ref.
func (g Git) Fetch(ctx context.Context, r *Repository, base string) (fetched bool, warn error, err error) {
	if err := g.validateBaseBranch(ctx, r.Path, base); err != nil {
		return false, nil, fmt.Errorf("fetch origin/%s for %s: %w", base, r.Name, err)
	}
	_, fetchErr := g.Run(ctx, r.Path, "fetch", "--no-tags", "origin", base)
	if fetchErr == nil {
		return true, nil, nil
	}
	fetchErr = fmt.Errorf("fetch origin/%s for %s: %w", base, r.Name, fetchErr)
	// A cancelled attempt must not carry on with a stale ref.
	if ctx.Err() != nil {
		return false, nil, fetchErr
	}
	exists, err := g.refExists(ctx, r.Path, "refs/remotes/origin/"+base)
	if err != nil {
		return false, nil, fmt.Errorf("%w; and checking the local ref failed: %w", fetchErr, err)
	}
	if !exists {
		return false, nil, fmt.Errorf("%w; and refs/remotes/origin/%s does not exist locally", fetchErr, base)
	}
	return false, fetchErr, nil
}

// refExists reports whether ref is present, using for-each-ref because it exits 0
// either way and so keeps "missing" distinct from "git failed".
func (g Git) refExists(ctx context.Context, dir, ref string) (bool, error) {
	out, err := g.Run(ctx, dir, "for-each-ref", "--format=%(refname)", ref)
	if err != nil {
		return false, err
	}
	for _, line := range strings.Split(out, "\n") {
		if strings.TrimSpace(line) == ref {
			return true, nil
		}
	}
	return false, nil
}

// ResolveBase picks the base branch (configured → forge.toml → origin/HEAD →
// error) and pins the base commit from the local remote-tracking ref, which
// Fetch has just refreshed. Only a full commit ID is accepted because it is
// stored and compared for the life of the attempt.
func (g Git) ResolveBase(ctx context.Context, r *Repository, forgeTomlBase string) (branch, commit string, err error) {
	branch = r.BaseBranch
	source := "base_branch"
	if branch == "" {
		branch = forgeTomlBase
		source = "forge.toml"
	}
	if branch == "" {
		out, err := g.Run(ctx, r.Path, "symbolic-ref", "--short", "refs/remotes/origin/HEAD")
		if err != nil {
			return "", "", fmt.Errorf("resolve base for %s: origin/HEAD is not known; set base_branch for %s: %w", r.Name, r.Name, err)
		}
		branch = strings.TrimPrefix(strings.TrimSpace(out), "origin/")
		source = "origin/HEAD"
	}
	if err := g.validateBaseBranch(ctx, r.Path, branch); err != nil {
		return "", "", fmt.Errorf("resolve base for %s (from %s): %w", r.Name, source, err)
	}
	out, err := g.Run(ctx, r.Path, "rev-parse", "--verify", "refs/remotes/origin/"+branch+"^{commit}")
	if err != nil {
		return "", "", fmt.Errorf("resolve base for %s: origin/%s (from %s): %w", r.Name, branch, source, err)
	}
	commit = strings.TrimSpace(out)
	if !isFullCommit(commit) {
		return "", "", fmt.Errorf("resolve base for %s: origin/%s did not resolve to a full commit ID (%q)", r.Name, branch, commit)
	}
	return branch, commit, nil
}

// WorktreeAdd creates a linked worktree on a new branch at commit. Starting from a
// raw commit with -b means the checkout's HEAD, index and working tree are never
// consulted or changed; only .git/worktrees/<name> and the new branch appear.
func (g Git) WorktreeAdd(ctx context.Context, r *Repository, path, branch, commit string) error {
	fail := func(err error) error { return fmt.Errorf("add worktree %s for %s: %w", path, r.Name, err) }
	if !isFullCommit(commit) {
		return fail(fmt.Errorf("base commit %q is not a full commit ID", commit))
	}
	if branch == "" || strings.HasPrefix(branch, "-") {
		return fail(fmt.Errorf("branch %q is not a valid branch name", branch))
	}
	if !filepath.IsAbs(path) {
		return fail(errors.New("path must be absolute"))
	}
	parent := filepath.Dir(path)
	info, err := os.Lstat(parent)
	if err != nil {
		return fail(fmt.Errorf("inspect parent directory: %w", err))
	}
	if info.Mode()&os.ModeSymlink != 0 {
		return fail(fmt.Errorf("parent %s is a symlink; the worktree root must be a real directory", parent))
	}
	if !info.IsDir() {
		return fail(fmt.Errorf("parent %s is not a directory", parent))
	}
	if _, err := os.Lstat(path); err == nil {
		return fail(errors.New("path already exists"))
	} else if !errors.Is(err, os.ErrNotExist) {
		return fail(fmt.Errorf("inspect path: %w", err))
	}
	if _, err := g.Run(ctx, r.Path, "worktree", "add", "-b", branch, path, commit); err != nil {
		return fail(err)
	}
	return nil
}

// WorktreeRemove removes a linked worktree and then proves it: git has reported
// success while leaving the directory or the registration behind, and a cleanup
// that only claims to have happened would leak worktrees and confuse reconcile.
// force is reserved for operator-confirmed cleanup (DESIGN §6).
func (g Git) WorktreeRemove(ctx context.Context, r *Repository, path string, force bool) error {
	args := []string{"worktree", "remove"}
	if force {
		args = append(args, "--force")
	}
	args = append(args, path)
	if _, err := g.Run(ctx, r.Path, args...); err != nil {
		return fmt.Errorf("remove worktree %s for %s: %w", path, r.Name, err)
	}
	state, err := g.WorktreeState(ctx, r, path)
	if err != nil {
		return fmt.Errorf("verify removal of worktree %s for %s: %w", path, r.Name, err)
	}
	if state.PathExists {
		return fmt.Errorf("remove worktree %s for %s: git reported success but the path remains", path, r.Name)
	}
	if state.Registered {
		return fmt.Errorf("remove worktree %s for %s: git reported success but the registration remains", path, r.Name)
	}
	return nil
}

// WorktreeState reports the filesystem and registry views of path separately so
// the cleanup rule can tell "gone", "half gone" and "present" apart. Two registry
// entries for one path is corruption and is reported, never resolved by guessing.
func (g Git) WorktreeState(ctx context.Context, r *Repository, path string) (WorktreeState, error) {
	var state WorktreeState
	// git prints the path as it stored it, which may be the canonical form; accept
	// every spelling that denotes this directory.
	candidates := map[string]bool{filepath.Clean(path): true}
	if parent, err := filepath.EvalSymlinks(filepath.Dir(path)); err == nil {
		candidates[filepath.Join(parent, filepath.Base(path))] = true
	}
	if _, err := os.Lstat(path); err == nil {
		state.PathExists = true
		if canonical, err := filepath.EvalSymlinks(path); err == nil {
			candidates[canonical] = true
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return state, fmt.Errorf("inspect worktree path %s: %w", path, err)
	}
	entries, err := g.listWorktrees(ctx, r.Path)
	if err != nil {
		return state, fmt.Errorf("list worktrees of %s: %w", r.Name, err)
	}
	for _, e := range entries {
		if !candidates[e.Path] {
			continue
		}
		if state.Registered {
			return state, fmt.Errorf("worktree %s: git lists it more than once", path)
		}
		state.Registered = true
		state.Branch = e.Branch
		state.Head = e.Head
	}
	return state, nil
}

// worktreeEntry is one record of `git worktree list --porcelain`.
type worktreeEntry struct {
	Path, Head, Branch string
}

func (g Git) listWorktrees(ctx context.Context, dir string) ([]worktreeEntry, error) {
	out, err := g.Run(ctx, dir, "worktree", "list", "--porcelain", "-z")
	if err != nil {
		return nil, err
	}
	return parseWorktreeList(out), nil
}

// parseWorktreeList reads the NUL-terminated porcelain format: records of
// "key value" lines, each line NUL-terminated, records separated by an empty line.
func parseWorktreeList(out string) []worktreeEntry {
	var entries []worktreeEntry
	var current worktreeEntry
	open := false
	flush := func() {
		if open {
			entries = append(entries, current)
		}
		current = worktreeEntry{}
		open = false
	}
	for _, line := range strings.Split(out, "\x00") {
		if line == "" {
			flush()
			continue
		}
		key, value, _ := strings.Cut(line, " ")
		switch key {
		case "worktree":
			flush()
			current.Path = value
			open = true
		case "HEAD":
			current.Head = value
		case "branch":
			current.Branch = strings.TrimPrefix(value, "refs/heads/")
		}
	}
	flush()
	return entries
}

// Inspect measures a worktree after the agent exits (DESIGN §5.7). It only reads:
// --no-optional-locks keeps status from writing index.lock into a tree the
// cleanup rule is about to judge. base may be a commit ID or a ref.
func (g Git) Inspect(ctx context.Context, worktree, base string) (protocol.GitOutcome, error) {
	var outcome protocol.GitOutcome
	fail := func(step string, err error) (protocol.GitOutcome, error) {
		return protocol.GitOutcome{}, fmt.Errorf("inspect worktree %s: %s: %w", worktree, step, err)
	}
	status, err := g.Run(ctx, worktree, "--no-optional-locks", "status", "--porcelain=v1", "-z")
	if err != nil {
		return fail("status", err)
	}
	outcome.Dirty = status != ""

	out, err := g.Run(ctx, worktree, "rev-parse", "--verify", "HEAD^{commit}")
	if err != nil {
		return fail("resolve HEAD", err)
	}
	outcome.Head = strings.TrimSpace(out)
	if !isFullCommit(outcome.Head) {
		return fail("resolve HEAD", fmt.Errorf("%q is not a full commit ID", outcome.Head))
	}
	out, err = g.Run(ctx, worktree, "rev-parse", "--verify", base+"^{commit}")
	if err != nil {
		return fail("resolve base "+base, err)
	}
	baseCommit := strings.TrimSpace(out)
	if !isFullCommit(baseCommit) {
		return fail("resolve base "+base, fmt.Errorf("%q is not a full commit ID", baseCommit))
	}
	rangeSpec := baseCommit + ".." + outcome.Head

	out, err = g.Run(ctx, worktree, "rev-list", "--count", rangeSpec)
	if err != nil {
		return fail("count commits", err)
	}
	outcome.Commits, err = strconv.Atoi(strings.TrimSpace(out))
	if err != nil {
		return fail("count commits", fmt.Errorf("parse %q: %w", strings.TrimSpace(out), err))
	}

	out, err = g.Run(ctx, worktree, "diff", "--numstat", rangeSpec)
	if err != nil {
		return fail("diff numstat", err)
	}
	outcome.FilesChanged, outcome.Insertions, outcome.Deletions, err = parseNumstat(out)
	if err != nil {
		return fail("diff numstat", err)
	}

	out, err = g.Run(ctx, worktree, "diff", "--name-status", "-z", rangeSpec)
	if err != nil {
		return fail("diff name-status", err)
	}
	paths := append(parseNameStatusZ(out), parseStatusZ(status)...)
	sort.Strings(paths)
	outcome.ChangedPaths = slices.Compact(paths)

	outcome.Pushed = outcome.Head == baseCommit
	if !outcome.Pushed {
		out, err = g.Run(ctx, worktree, "for-each-ref", "--format=%(refname)", "--contains", outcome.Head, "refs/remotes")
		if err != nil {
			return fail("find remote refs containing HEAD", err)
		}
		outcome.Pushed = strings.TrimSpace(out) != ""
	}
	return outcome, nil
}

// parseNumstat sums `git diff --numstat` lines; binary files print "-" and count
// as a changed file with no line counts.
func parseNumstat(out string) (files, insertions, deletions int, err error) {
	for _, line := range strings.Split(out, "\n") {
		if strings.TrimSpace(line) == "" {
			continue
		}
		fields := strings.SplitN(line, "\t", 3)
		if len(fields) != 3 {
			return 0, 0, 0, fmt.Errorf("unexpected numstat line %q", line)
		}
		files++
		if fields[0] == "-" && fields[1] == "-" {
			continue
		}
		ins, err := strconv.Atoi(fields[0])
		if err != nil {
			return 0, 0, 0, fmt.Errorf("parse numstat line %q: %w", line, err)
		}
		del, err := strconv.Atoi(fields[1])
		if err != nil {
			return 0, 0, 0, fmt.Errorf("parse numstat line %q: %w", line, err)
		}
		insertions += ins
		deletions += del
	}
	return files, insertions, deletions, nil
}

// parseNameStatusZ lists every path in `git diff --name-status -z` output; a
// rename or copy contributes both its old and new path.
func parseNameStatusZ(out string) []string {
	var paths []string
	tokens := strings.Split(out, "\x00")
	for i := 0; i < len(tokens); i++ {
		status := tokens[i]
		if status == "" {
			continue
		}
		if i+1 >= len(tokens) {
			break
		}
		i++
		paths = append(paths, tokens[i])
		if (status[0] == 'R' || status[0] == 'C') && i+1 < len(tokens) {
			i++
			paths = append(paths, tokens[i])
		}
	}
	return paths
}

// parseStatusZ lists every path in `git status --porcelain=v1 -z` output ("XY
// path", with a staged rename's original path in the following entry).
func parseStatusZ(out string) []string {
	var paths []string
	tokens := strings.Split(out, "\x00")
	for i := 0; i < len(tokens); i++ {
		entry := tokens[i]
		if len(entry) < 4 {
			continue
		}
		paths = append(paths, entry[3:])
		if (entry[0] == 'R' || entry[0] == 'C') && i+1 < len(tokens) {
			i++
			paths = append(paths, tokens[i])
		}
	}
	return paths
}

// GitConfigEnv renders git options as GIT_CONFIG_COUNT/GIT_CONFIG_KEY_n/
// GIT_CONFIG_VALUE_n entries, sorted by key so the environment is reproducible.
// Forge configures its worktrees this way and never writes .git/config (STYLE §10).
func GitConfigEnv(options map[string]string) []string {
	if len(options) == 0 {
		return nil
	}
	keys := slices.Sorted(maps.Keys(options))
	env := make([]string, 0, 1+2*len(keys))
	env = append(env, fmt.Sprintf("GIT_CONFIG_COUNT=%d", len(keys)))
	for i, key := range keys {
		env = append(env,
			fmt.Sprintf("GIT_CONFIG_KEY_%d=%s", i, key),
			fmt.Sprintf("GIT_CONFIG_VALUE_%d=%s", i, options[key]))
	}
	return env
}
