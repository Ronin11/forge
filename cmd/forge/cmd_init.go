package main

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/BurntSushi/toml"

	"forge/internal/core/config"
	"forge/internal/core/doctor"
	"forge/internal/core/worker"
)

// runInit is the optional interactive half of setup (DESIGN.md §1.3): report
// binaries, register repositories, choose the kb path, and — behind flags —
// install the systemd units and Playwright. Bootstrap proper stays in the
// daemon; init only edits the same files and says what it did or skipped.
func runInit(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("init")
	yes := fs.Bool("yes", false, "take every default; no prompts")
	service := fs.Bool("service", false, "install the systemd user units")
	withBrowser := fs.Bool("with-browser", false, "install Playwright under <home>/deps (never global)")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.stderr, "forge init: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.init")
	if code >= 0 {
		return code
	}
	log.DebugContext(ctx, "init", "yes", *yes, "service", *service, "with_browser", *withBrowser, "home", c.forgeHome)
	if err := os.MkdirAll(c.forgeHome, 0o700); err != nil {
		return c.fail("init", fmt.Errorf("create %s: %w", c.forgeHome, err))
	}

	// Step 1: binaries. Same checks as doctor, so the two never disagree.
	fmt.Fprintln(c.stdout, "binaries:")
	for _, ch := range doctor.Binaries(exec.LookPath, binaryVersion) {
		fmt.Fprintf(c.stdout, "  %-4s %-6s %s\n", ch.Status, strings.TrimPrefix(ch.Name, "binary."), ch.Detail)
	}

	cfg, err := config.LoadConfig(filepath.Join(c.forgeHome, "config.toml"), c.forgeHome, c.userHome, c.getenv)
	if err != nil {
		return c.fail("init", err)
	}

	// Step 2: repositories.
	if code := initRepositories(ctx, c, cfg.Repositories.ProjectsRoot, *yes); code != 0 {
		return code
	}

	// Step 3: kb path.
	if *yes {
		fmt.Fprintf(c.stdout, "kb path: %s (kept; --yes)\n", cfg.KB.Path)
	} else if code := initKbPath(c, cfg.KB.Path); code != 0 {
		return code
	}

	// Step 4: systemd units, through the same code path as `forge service install`.
	if !*service {
		fmt.Fprintln(c.stdout, "service: skipped (run with --service to install the systemd user units)")
	} else {
		if _, err := exec.LookPath("systemctl"); err != nil {
			fmt.Fprintln(c.stderr, "forge init: systemctl is not on PATH — cannot install the user units")
			return 1
		}
		self, err := os.Executable()
		if err != nil {
			return c.fail("init", fmt.Errorf("resolve forge binary: %w", err))
		}
		if code := serviceInstall(ctx, c, serviceUnitDir(c), self, execServiceRunner); code != 0 {
			return code
		}
	}

	// Step 5: Playwright under <home>/deps.
	if !*withBrowser {
		fmt.Fprintln(c.stdout, "browser: skipped (run with --with-browser to install Playwright)")
	} else if code := initBrowser(ctx, c); code != 0 {
		return code
	}
	return 0
}

// repoCandidate is one Git checkout with an origin remote found under the
// projects root.
type repoCandidate struct {
	Name, Path, Origin string
	Forge              bool // the checkout the running binary lives in; pre-checked
}

// initRepositories scans <projects_root>/* for checkouts with an origin and
// offers to register each in worker.toml. With --yes nothing is written: the
// safe default keeps worker.toml as-is and reports what an interactive run
// would offer.
func initRepositories(ctx context.Context, c *cmdContext, projectsRoot string, yes bool) int {
	candidates := scanRepos(ctx, projectsRoot, forgeCheckout())
	wtPath := filepath.Join(c.forgeHome, "worker.toml")
	registered := registeredRepoNames(wtPath)
	var fresh []repoCandidate
	for _, cand := range candidates {
		if !registered[cand.Name] {
			fresh = append(fresh, cand)
		}
	}
	fmt.Fprintf(c.stdout, "repositories: %d checkouts with an origin under %s, %d already in worker.toml\n", len(candidates), projectsRoot, len(candidates)-len(fresh))
	if len(fresh) == 0 {
		fmt.Fprintln(c.stdout, "  nothing new to register")
		return 0
	}
	if yes {
		for _, cand := range fresh {
			fmt.Fprintf(c.stdout, "  would register %s (%s)\n", cand.Name, cand.Path)
		}
		fmt.Fprintln(c.stdout, "  worker.toml left unchanged (--yes registers nothing; run interactively to choose)")
		return 0
	}
	in := promptReader(c)
	adds := map[string]string{}
	for _, cand := range fresh {
		if askYesNo(c, in, fmt.Sprintf("  register %s (%s)?", cand.Name, cand.Path), cand.Forge) {
			adds[cand.Name] = cand.Path
		}
	}
	if len(adds) == 0 {
		fmt.Fprintln(c.stdout, "  no repositories registered")
		return 0
	}
	// A fresh box has no worker.toml yet; seed the same default bootstrap writes.
	self, err := os.Executable()
	if err != nil {
		return c.fail("init", fmt.Errorf("resolve forge binary: %w", err))
	}
	if _, err := worker.WriteDefault(wtPath, c.forgeHome, self); err != nil {
		return c.fail("init", err)
	}
	if err := mergeTomlFile(wtPath, func(m map[string]any) {
		repos, ok := m["repositories"].(map[string]any)
		if !ok || repos == nil {
			repos = map[string]any{}
		}
		for name, path := range adds {
			if _, exists := repos[name]; !exists {
				repos[name] = map[string]any{"path": path}
			}
		}
		m["repositories"] = repos
	}); err != nil {
		return c.fail("init", err)
	}
	if _, err := worker.LoadConfig(wtPath); err != nil {
		return c.fail("init", fmt.Errorf("worker.toml is invalid after the merge: %w", err))
	}
	names := make([]string, 0, len(adds))
	for name := range adds {
		names = append(names, name)
	}
	sort.Strings(names)
	fmt.Fprintf(c.stdout, "  registered %s in %s (the worker advertises them within 30 s; 'forge daemon restart' if it is not running)\n", strings.Join(names, ", "), wtPath)
	return 0
}

// initKbPath shows the current [kb] path and rewrites config.toml if the
// operator types a different one.
func initKbPath(c *cmdContext, current string) int {
	in := promptReader(c)
	if in == nil {
		fmt.Fprintf(c.stdout, "kb path: %s (kept; no terminal)\n", current)
		return 0
	}
	fmt.Fprintf(c.stdout, "kb path [%s]: ", current)
	line, err := in.ReadString('\n')
	if err != nil && line == "" {
		fmt.Fprintf(c.stdout, "\nkb path: %s (kept)\n", current)
		return 0
	}
	answer := strings.TrimSpace(line)
	if answer == "" || answer == current {
		fmt.Fprintf(c.stdout, "kb path: %s (kept)\n", current)
		return 0
	}
	cfgPath := filepath.Join(c.forgeHome, "config.toml")
	if _, err := config.WriteDefaultConfig(cfgPath, c.forgeHome, c.userHome); err != nil {
		return c.fail("init", err)
	}
	if err := mergeTomlFile(cfgPath, func(m map[string]any) {
		kb, ok := m["kb"].(map[string]any)
		if !ok || kb == nil {
			kb = map[string]any{}
		}
		kb["path"] = answer
		m["kb"] = kb
	}); err != nil {
		return c.fail("init", err)
	}
	fmt.Fprintf(c.stdout, "kb path: set to %s (the daemon reads config.toml on restart)\n", answer)
	return 0
}

// initBrowser installs Playwright and Chromium under <home>/deps — never
// globally — streaming npm's output so failures are the real ones.
func initBrowser(ctx context.Context, c *cmdContext) int {
	deps := filepath.Join(c.forgeHome, "deps")
	if err := os.MkdirAll(deps, 0o700); err != nil {
		return c.fail("init", fmt.Errorf("create %s: %w", deps, err))
	}
	for _, argv := range [][]string{
		{"npm", "i", "playwright"},
		{"npx", "playwright", "install", "chromium"},
	} {
		fmt.Fprintf(c.stdout, "browser: running %s in %s\n", strings.Join(argv, " "), deps)
		cmd := exec.CommandContext(ctx, argv[0], argv[1:]...)
		cmd.Dir = deps
		cmd.Stdout, cmd.Stderr = c.stdout, c.stderr
		if err := cmd.Run(); err != nil {
			return c.fail("init", fmt.Errorf("%s: %w", strings.Join(argv, " "), err))
		}
	}
	fmt.Fprintf(c.stdout, "browser: installed under %s (the worker advertises browser=ready on its next registration)\n", deps)
	return 0
}

// scanRepos lists <root>/* directories whose `git remote get-url origin`
// answers; forgeDir marks the checkout the running binary belongs to.
func scanRepos(ctx context.Context, root, forgeDir string) []repoCandidate {
	entries, err := os.ReadDir(root)
	if err != nil {
		return nil
	}
	var out []repoCandidate
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		path := filepath.Join(root, e.Name())
		cctx, cancel := context.WithTimeout(ctx, 2*time.Second)
		origin, err := exec.CommandContext(cctx, "git", "-C", path, "remote", "get-url", "origin").Output()
		cancel()
		if err != nil {
			continue
		}
		out = append(out, repoCandidate{Name: e.Name(), Path: path, Origin: strings.TrimSpace(string(origin)), Forge: path == forgeDir})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// forgeCheckout walks up from the running binary looking for a .git directory:
// the Forge repository itself, pre-checked in the offer list.
func forgeCheckout() string {
	self, err := os.Executable()
	if err != nil {
		return ""
	}
	for dir := filepath.Dir(self); ; dir = filepath.Dir(dir) {
		if _, err := os.Stat(filepath.Join(dir, ".git")); err == nil {
			return dir
		}
		if dir == filepath.Dir(dir) {
			return ""
		}
	}
}

// registeredRepoNames reads worker.toml leniently: init must not refuse to run
// because a config it is about to fix is odd; a missing file is no names.
func registeredRepoNames(path string) map[string]bool {
	out := map[string]bool{}
	var raw struct {
		Repositories map[string]struct {
			Path string `toml:"path"`
		} `toml:"repositories"`
	}
	if _, err := toml.DecodeFile(path, &raw); err != nil {
		return out
	}
	for name := range raw.Repositories {
		out[name] = true
	}
	return out
}

// mergeTomlFile rewrites one TOML file through a mutation of its decoded map,
// preserving every existing entry (comments are lost; the values are not).
func mergeTomlFile(path string, mutate func(m map[string]any)) error {
	m := map[string]any{}
	if _, err := os.Stat(path); err == nil {
		if _, err := toml.DecodeFile(path, &m); err != nil {
			return fmt.Errorf("read %s: %w", path, err)
		}
	} else if !os.IsNotExist(err) {
		return fmt.Errorf("stat %s: %w", path, err)
	}
	mutate(m)
	var b strings.Builder
	if err := toml.NewEncoder(&b).Encode(m); err != nil {
		return fmt.Errorf("encode %s: %w", path, err)
	}
	if err := os.WriteFile(path, []byte(b.String()), 0o600); err != nil {
		return fmt.Errorf("write %s: %w", path, err)
	}
	return nil
}

// promptReader wraps c.stdin for prompting; nil when there is nothing to read
// from, in which case every prompt takes its default.
func promptReader(c *cmdContext) *bufio.Reader {
	if c.stdin == nil {
		return nil
	}
	return bufio.NewReader(c.stdin)
}

// askYesNo prompts with the default capitalised and reads one line; EOF, a
// missing stdin, or a blank answer takes the default.
func askYesNo(c *cmdContext, in *bufio.Reader, prompt string, def bool) bool {
	suffix := " [y/N] "
	if def {
		suffix = " [Y/n] "
	}
	if in == nil {
		return def
	}
	fmt.Fprint(c.stdout, prompt+suffix)
	line, err := in.ReadString('\n')
	answer := strings.ToLower(strings.TrimSpace(line))
	if err != nil && answer == "" {
		fmt.Fprintln(c.stdout)
		return def
	}
	switch answer {
	case "":
		return def
	case "y", "yes":
		return true
	default:
		return false
	}
}
