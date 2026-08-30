package worker

import (
	"context"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
)

var commitPattern = regexp.MustCompile(`^[0-9a-f]{40}$`)

// Repository is a validated, registered checkout.
type Repository struct {
	Name           string
	Path           string // canonical absolute path
	RemoteIdentity string // normalised origin, e.g. github.com/owner/repo
	BaseBranch     string // short name, e.g. master
}

// ValidateRepository checks that path is a non-bare checkout with an origin
// and resolves the base branch (configured or origin's default).
func ValidateRepository(ctx context.Context, name, path, baseBranch string) (Repository, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return Repository{}, err
	}
	canonical, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return Repository{}, fmt.Errorf("repository %q: %w", name, err)
	}
	out, err := git(ctx, canonical, "rev-parse", "--is-inside-work-tree")
	if err != nil {
		return Repository{}, fmt.Errorf("repository %q: %w", name, err)
	}
	if strings.TrimSpace(out) != "true" {
		return Repository{}, fmt.Errorf("repository %q: %s is not a non-bare checkout", name, canonical)
	}
	top, err := git(ctx, canonical, "rev-parse", "--show-toplevel")
	if err != nil {
		return Repository{}, fmt.Errorf("repository %q: %w", name, err)
	}
	if strings.TrimSpace(top) != canonical {
		return Repository{}, fmt.Errorf("repository %q: path must be the checkout root %s", name, strings.TrimSpace(top))
	}
	remote, err := git(ctx, canonical, "remote", "get-url", "origin")
	if err != nil {
		return Repository{}, fmt.Errorf("repository %q: origin remote is required: %w", name, err)
	}
	identity, err := normalizeRemoteIdentity(strings.TrimSpace(remote), canonical)
	if err != nil {
		return Repository{}, fmt.Errorf("repository %q: %w", name, err)
	}
	repo := Repository{Name: name, Path: canonical, RemoteIdentity: identity, BaseBranch: baseBranch}
	if repo.BaseBranch == "" {
		repo.BaseBranch, err = discoverDefaultBranch(ctx, canonical)
		if err != nil {
			return Repository{}, fmt.Errorf("repository %q: %w; set base_branch", name, err)
		}
	}
	if err := validateBranchName(ctx, canonical, repo.BaseBranch); err != nil {
		return Repository{}, fmt.Errorf("repository %q: %w", name, err)
	}
	if _, err := git(ctx, canonical, "rev-parse", "--verify", "--quiet", "refs/remotes/origin/"+repo.BaseBranch+"^{commit}"); err != nil {
		return Repository{}, fmt.Errorf("repository %q: base branch origin/%s is not resolvable (fetch first?)", name, repo.BaseBranch)
	}
	return repo, nil
}

func discoverDefaultBranch(ctx context.Context, dir string) (string, error) {
	out, err := git(ctx, dir, "symbolic-ref", "--short", "refs/remotes/origin/HEAD")
	if err == nil {
		if b, ok := strings.CutPrefix(strings.TrimSpace(out), "origin/"); ok && b != "" {
			return b, nil
		}
	}
	out, err = runCommand(ctx, fetchTimeout, dir, "git", "ls-remote", "--symref", "origin", "HEAD")
	if err != nil {
		return "", fmt.Errorf("discover origin default branch: %w", err)
	}
	for _, line := range strings.Split(out, "\n") {
		f := strings.Fields(line)
		if len(f) == 3 && f[0] == "ref:" && f[2] == "HEAD" {
			if b, ok := strings.CutPrefix(f[1], "refs/heads/"); ok {
				return b, nil
			}
		}
	}
	return "", errors.New("origin did not advertise a default branch")
}

func validateBranchName(ctx context.Context, dir, branch string) error {
	if branch == "HEAD" || strings.HasPrefix(branch, "refs/") || strings.HasPrefix(branch, "origin/") {
		return errors.New("base_branch must be a short branch name such as main")
	}
	if _, err := git(ctx, dir, "check-ref-format", "refs/heads/"+branch); err != nil {
		return fmt.Errorf("invalid base_branch %q", branch)
	}
	return nil
}

// normalizeRemoteIdentity turns any origin URL form into host/path or file://path.
func normalizeRemoteIdentity(remote, repoPath string) (string, error) {
	if remote == "" {
		return "", errors.New("origin remote is empty")
	}
	if prefix, path, found := strings.Cut(remote, ":"); found && strings.Contains(prefix, "@") && !strings.Contains(prefix, "/") {
		host := prefix[strings.LastIndex(prefix, "@")+1:]
		if host == "" || path == "" {
			return "", errors.New("origin SSH remote is malformed")
		}
		return hostPath(host, path), nil
	}
	if u, err := url.Parse(remote); err == nil && u.Scheme != "" {
		if u.Scheme == "file" {
			p, err := filepath.EvalSymlinks(u.Path)
			if err != nil {
				return "", err
			}
			return "file://" + p, nil
		}
		if u.Hostname() == "" || u.Path == "" {
			return "", errors.New("origin remote URL is malformed")
		}
		return hostPath(u.Host, u.Path), nil
	}
	p := remote
	if !filepath.IsAbs(p) {
		p = filepath.Join(repoPath, p)
	}
	p, err := filepath.EvalSymlinks(p)
	if err != nil {
		return "", err
	}
	return "file://" + p, nil
}

func hostPath(host, path string) string {
	path = strings.TrimSuffix(strings.TrimSuffix(strings.TrimPrefix(path, "/"), "/"), ".git")
	return strings.ToLower(host) + "/" + path
}

// checkOrigin fails if the checkout's origin no longer matches the pinned identity.
func (r Repository) checkOrigin(ctx context.Context) error {
	out, err := git(ctx, r.Path, "remote", "get-url", "origin")
	if err != nil {
		return err
	}
	id, err := normalizeRemoteIdentity(strings.TrimSpace(out), r.Path)
	if err != nil {
		return err
	}
	if !strings.EqualFold(id, r.RemoteIdentity) {
		return fmt.Errorf("repository %q: origin changed since registration (%s != %s)", r.Name, id, r.RemoteIdentity)
	}
	return nil
}

// FetchBase fetches the base branch from origin and returns its exact commit.
// It never changes the checkout's HEAD, index, or working tree.
func (r Repository) FetchBase(ctx context.Context) (string, error) {
	if err := r.checkOrigin(ctx); err != nil {
		return "", err
	}
	ref := "refs/heads/" + r.BaseBranch
	if _, err := runCommand(ctx, fetchTimeout, r.Path, "git", "fetch", "--no-tags", "origin", ref+":refs/remotes/origin/"+r.BaseBranch); err != nil {
		return "", fmt.Errorf("fetch origin/%s: %w", r.BaseBranch, err)
	}
	out, err := git(ctx, r.Path, "rev-parse", "--verify", "refs/remotes/origin/"+r.BaseBranch+"^{commit}")
	if err != nil {
		return "", err
	}
	commit := strings.TrimSpace(out)
	if !commitPattern.MatchString(commit) {
		return "", fmt.Errorf("origin/%s did not resolve to a full commit: %q", r.BaseBranch, commit)
	}
	if err := r.checkOrigin(ctx); err != nil {
		return "", err
	}
	return commit, nil
}

// AddWorktree creates a worktree at path on a new branch at commit.
func (r Repository) AddWorktree(ctx context.Context, path, branch, commit string) error {
	if !commitPattern.MatchString(commit) {
		return errors.New("worktree base must be a full commit")
	}
	if _, err := os.Lstat(path); err == nil {
		return fmt.Errorf("worktree path %s already exists", path)
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	_, err := git(ctx, r.Path, "worktree", "add", "-b", branch, path, commit)
	return err
}

// WorktreeEntry is one line of `git worktree list`.
type WorktreeEntry struct {
	Path, Head, Branch string
}

// ListWorktrees returns every worktree registered with the checkout.
func (r Repository) ListWorktrees(ctx context.Context) ([]WorktreeEntry, error) {
	out, err := git(ctx, r.Path, "worktree", "list", "--porcelain", "-z")
	if err != nil {
		return nil, err
	}
	var entries []WorktreeEntry
	var cur WorktreeEntry
	for _, tok := range strings.Split(out, "\x00") {
		if tok == "" {
			if cur.Path != "" {
				entries = append(entries, cur)
				cur = WorktreeEntry{}
			}
			continue
		}
		key, value, _ := strings.Cut(tok, " ")
		switch key {
		case "worktree":
			cur.Path = value
		case "HEAD":
			cur.Head = value
		case "branch":
			cur.Branch = strings.TrimPrefix(value, "refs/heads/")
		}
	}
	if cur.Path != "" {
		entries = append(entries, cur)
	}
	return entries, nil
}

// WorktreeState is what Inspect saw.
type WorktreeState struct {
	Exists     bool // directory present
	Registered bool // known to `git worktree list`
	Branch     string
	Head       string
	Dirty      bool
	NewCommits int  // commits in base..HEAD
	Pushed     bool // HEAD contained in some refs/remotes/* ref
}

// InspectWorktree reports the Git state of a worktree relative to baseCommit.
func (r Repository) InspectWorktree(ctx context.Context, path, baseCommit string) (WorktreeState, error) {
	var s WorktreeState
	info, err := os.Lstat(path)
	switch {
	case err == nil:
		s.Exists = info.IsDir()
	case errors.Is(err, os.ErrNotExist):
	default:
		return s, err
	}
	entries, err := r.ListWorktrees(ctx)
	if err != nil {
		return s, err
	}
	for _, e := range entries {
		if filepath.Clean(e.Path) == filepath.Clean(path) {
			s.Registered, s.Branch, s.Head = true, e.Branch, e.Head
		}
	}
	if !s.Exists || !s.Registered {
		return s, nil
	}
	out, err := git(ctx, path, "--no-optional-locks", "status", "--porcelain")
	if err != nil {
		return s, err
	}
	s.Dirty = strings.TrimSpace(out) != ""
	if out, err = git(ctx, path, "rev-parse", "HEAD"); err != nil {
		return s, err
	}
	s.Head = strings.TrimSpace(out)
	if baseCommit != "" && s.Head != baseCommit {
		out, err = git(ctx, path, "rev-list", "--count", baseCommit+".."+s.Head)
		if err != nil {
			return s, err
		}
		s.NewCommits, _ = strconv.Atoi(strings.TrimSpace(out))
		out, err = git(ctx, path, "for-each-ref", "--format=%(refname)", "--contains", s.Head, "refs/remotes")
		if err != nil {
			return s, err
		}
		s.Pushed = strings.TrimSpace(out) != ""
	}
	return s, nil
}

// DecideCleanup applies the cleanup rules: remove only when the tree is clean
// and nothing beyond the base is unpublished.
func DecideCleanup(s WorktreeState) (remove bool, reason string) {
	switch {
	case !s.Exists || !s.Registered:
		return false, "worktree missing"
	case s.Dirty:
		return false, "dirty worktree"
	case s.NewCommits > 0 && !s.Pushed:
		return false, "unpushed commits"
	default:
		return true, "clean"
	}
}

// RemoveWorktree removes a worktree (never its branch) and verifies it is gone.
func (r Repository) RemoveWorktree(ctx context.Context, path string, force bool) error {
	args := []string{"worktree", "remove"}
	if force {
		args = append(args, "--force")
	}
	if _, err := git(ctx, r.Path, append(args, path)...); err != nil {
		return err
	}
	if _, err := os.Lstat(path); !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("worktree path %s still exists after removal", path)
	}
	entries, err := r.ListWorktrees(ctx)
	if err != nil {
		return err
	}
	for _, e := range entries {
		if filepath.Clean(e.Path) == filepath.Clean(path) {
			return fmt.Errorf("worktree %s still registered after removal", path)
		}
	}
	return nil
}
