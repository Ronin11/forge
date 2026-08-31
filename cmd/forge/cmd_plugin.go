package main

import (
	"bufio"
	"context"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"text/tabwriter"
	"time"

	"forge/internal/plugin"
)

// pluginRow mirrors the daemon's GET /api/v1/plugins row.
type pluginRow struct {
	Name     string    `json:"name"`
	Version  string    `json:"version"`
	Kind     string    `json:"kind"`
	Enabled  bool      `json:"enabled"`
	Scopes   []string  `json:"scopes"`
	Cursor   int64     `json:"cursor"`
	Running  bool      `json:"running"`
	PID      int       `json:"pid"`
	Restarts int       `json:"restarts"`
	LastExit string    `json:"last_exit"`
	Since    time.Time `json:"since"`
}

func runPlugin(ctx context.Context, c *cmdContext, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		code := 2
		if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
			code = 0
		}
		fmt.Fprintln(c.stderr, "usage: forge plugin list|install NAME|uninstall NAME|enable NAME|disable NAME|logs NAME|status [flags]")
		return code
	}
	switch args[0] {
	case "list":
		return runPluginList(ctx, c, args[1:])
	case "install":
		return runPluginInstall(ctx, c, args[1:])
	case "uninstall":
		return runPluginUninstall(ctx, c, args[1:])
	case "enable":
		return runPluginEnable(ctx, c, args[1:])
	case "disable":
		return runPluginDisable(ctx, c, args[1:])
	case "logs":
		return runPluginLogs(ctx, c, args[1:])
	case "status":
		return runPluginStatus(ctx, c, args[1:])
	}
	fmt.Fprintf(c.stderr, "forge plugin: unknown subcommand %q\n", args[0])
	return 2
}

// pluginArg reads the one NAME positional every mutating subcommand takes.
func pluginArg(c *cmdContext, name string, fsArgs []string) (string, int) {
	if len(fsArgs) != 1 {
		fmt.Fprintf(c.stderr, "usage: forge plugin %s NAME\n", name)
		return "", 2
	}
	return fsArgs[0], -1
}

func runPluginList(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("plugin list")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.plugin")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("plugin list", err)
	}
	var rows []pluginRow
	if err := cl.do(ctx, http.MethodGet, "/api/v1/plugins", nil, &rows); err != nil {
		return c.fail("plugin list", err)
	}
	// Merge in discovered-but-uninstalled manifests: the repo's first-party
	// plugins (install sources) and anything dropped under <home>/plugins.
	roots := []string{filepath.Join(c.forgeHome, "plugins")}
	if repoRoot, err := findRepoPluginsRoot(); err == nil {
		roots = append([]string{repoRoot}, roots...)
	}
	installed := map[string]bool{}
	for _, r := range rows {
		installed[r.Name] = true
	}
	for _, m := range plugin.Discover(roots, nil) {
		if !installed[m.Name] {
			rows = append(rows, pluginRow{Name: m.Name, Version: m.Version, Kind: "available"})
		}
	}
	if *asJSON {
		c.printJSON(rows)
		return 0
	}
	tw := tabwriter.NewWriter(c.stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "NAME\tVERSION\tKIND\tENABLED\tRUNNING\tRESTARTS")
	for _, r := range rows {
		enabled, running := "no", "no"
		if r.Enabled {
			enabled = "yes"
		}
		if r.Running {
			running = "yes"
		}
		if r.Kind == "available" {
			enabled, running = "-", "-"
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%d\n", r.Name, r.Version, r.Kind, enabled, running, r.Restarts)
	}
	if err := tw.Flush(); err != nil {
		return c.fail("plugin list", err)
	}
	return 0
}

func runPluginInstall(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("plugin install")
	force := fs.Bool("force", false, "overwrite an existing installed copy")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	name, code := pluginArg(c, "install", fs.Args())
	if code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.plugin")
	if code >= 0 {
		return code
	}
	if name == "omarchy-indicator" {
		// The Omarchy half installs a waybar module besides the Forge plugin;
		// its own file owns that flow.
		if err := installOmarchyIndicator(ctx, c); err != nil {
			return c.fail("plugin install", err)
		}
		return 0
	}
	src, err := findRepoPluginDir(name)
	if err != nil {
		return c.fail("plugin install", err)
	}
	dst := filepath.Join(c.forgeHome, "plugins", name)
	if _, err := os.Stat(dst); err == nil {
		if !*force {
			return c.fail("plugin install", fmt.Errorf("%s already exists; --force replaces it", dst))
		}
		if err := os.RemoveAll(dst); err != nil {
			return c.fail("plugin install", err)
		}
	}
	if err := os.MkdirAll(filepath.Dir(dst), 0o700); err != nil {
		return c.fail("plugin install", err)
	}
	m, err := plugin.Load(src)
	if err != nil {
		return c.fail("plugin install", err)
	}
	if len(m.Build) > 0 {
		// Build in the SOURCE checkout: first-party Go plugins are part of the
		// repo module and cannot build from the copied dir (no go.mod there —
		// M7 smoke 1). The built binary ships with the copy.
		fmt.Fprintf(c.stdout, "building %s: %s\n", name, strings.Join(m.Build, " "))
		if err := runPluginBuild(ctx, c, src, m.Build); err != nil {
			return c.fail("plugin install", err)
		}
	}
	if err := os.CopyFS(dst, os.DirFS(src)); err != nil {
		return c.fail("plugin install", fmt.Errorf("copy %s: %w", src, err))
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("plugin install", err)
	}
	var row pluginRow
	body := map[string]string{"name": name, "kind": "first_party"}
	if err := cl.do(ctx, http.MethodPost, "/api/v1/plugins/install", body, &row); err != nil {
		return c.fail("plugin install", err)
	}
	fmt.Fprintf(c.stdout, "plugin %s %s installed — enable it with 'forge plugin enable %s'\n", row.Name, row.Version, row.Name)
	return 0
}

// runPluginBuild runs the manifest's Build argv in the installed copy,
// streaming its output; first-party Go plugins compile themselves here.
func runPluginBuild(ctx context.Context, c *cmdContext, dir string, build []string) error {
	argv := append([]string(nil), build...)
	if filepath.IsAbs(argv[0]) || strings.Contains(argv[0], "/") {
		if !filepath.IsAbs(argv[0]) {
			argv[0] = filepath.Join(dir, argv[0])
		}
	}
	cmd := exec.CommandContext(ctx, argv[0], argv[1:]...)
	cmd.Dir = dir
	cmd.Stdout, cmd.Stderr = c.stdout, c.stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("build %s: %w", strings.Join(build, " "), err)
	}
	return nil
}

func runPluginUninstall(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("plugin uninstall")
	keepFiles := fs.Bool("keep-files", false, "leave <home>/plugins/NAME in place")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	name, code := pluginArg(c, "uninstall", fs.Args())
	if code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.plugin")
	if code >= 0 {
		return code
	}
	if name == "omarchy-indicator" {
		if err := uninstallOmarchyIndicator(ctx, c); err != nil {
			return c.fail("plugin uninstall", err)
		}
		return 0
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("plugin uninstall", err)
	}
	if err := cl.do(ctx, http.MethodDelete, "/api/v1/plugins/"+name, nil, nil); err != nil {
		return c.fail("plugin uninstall", err)
	}
	if !*keepFiles {
		if err := os.RemoveAll(filepath.Join(c.forgeHome, "plugins", name)); err != nil {
			return c.fail("plugin uninstall", err)
		}
	}
	fmt.Fprintf(c.stdout, "plugin %s uninstalled\n", name)
	return 0
}

func runPluginEnable(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("plugin enable")
	yes := fs.Bool("yes", false, "skip the scope approval prompt")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	name, code := pluginArg(c, "enable", fs.Args())
	if code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.plugin")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("plugin enable", err)
	}
	var row pluginRow
	if err := cl.do(ctx, http.MethodGet, "/api/v1/plugins/"+name, nil, &row); err != nil {
		return c.fail("plugin enable", err)
	}
	scopes := "none (no API access)"
	if len(row.Scopes) > 0 {
		scopes = strings.Join(row.Scopes, ", ")
	}
	fmt.Fprintf(c.stdout, "enabling %s grants: %s\n", name, scopes)
	if !*yes && !confirm(c, "enable? [y/N] ") {
		fmt.Fprintln(c.stdout, "not enabled")
		return 1
	}
	if err := cl.do(ctx, http.MethodPost, "/api/v1/plugins/"+name+"/enable", struct{}{}, &row); err != nil {
		return c.fail("plugin enable", err)
	}
	fmt.Fprintf(c.stdout, "plugin %s enabled\n", name)
	if strings.HasPrefix(row.LastExit, "start failed") {
		fmt.Fprintf(c.stderr, "warning: %s — see forge plugin logs %s\n", row.LastExit, name)
	}
	for _, s := range row.Scopes {
		if s == plugin.ScopeToolsProvide {
			fmt.Fprintln(c.stdout, "note: its MCP tools appear to agents after the next daemon restart (forge daemon restart)")
		}
	}
	return 0
}

// confirm asks one y/N question on c.stdin; no stdin means no.
func confirm(c *cmdContext, prompt string) bool {
	fmt.Fprint(c.stdout, prompt)
	if c.stdin == nil {
		fmt.Fprintln(c.stdout)
		return false
	}
	line, err := bufio.NewReader(c.stdin).ReadString('\n')
	if err != nil && line == "" {
		return false
	}
	switch strings.ToLower(strings.TrimSpace(line)) {
	case "y", "yes":
		return true
	}
	return false
}

func runPluginDisable(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("plugin disable")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	name, code := pluginArg(c, "disable", fs.Args())
	if code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.plugin")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("plugin disable", err)
	}
	if err := cl.do(ctx, http.MethodPost, "/api/v1/plugins/"+name+"/disable", struct{}{}, nil); err != nil {
		return c.fail("plugin disable", err)
	}
	fmt.Fprintf(c.stdout, "plugin %s disabled (its tools, if any, vanish at the next daemon restart)\n", name)
	return 0
}

func runPluginLogs(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("plugin logs")
	n := fs.Int("n", 50, "lines to show")
	follow := fs.Bool("f", false, "keep printing as the plugin logs (Ctrl-C exits)")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	name, code := pluginArg(c, "logs", fs.Args())
	if code >= 0 {
		return code
	}
	if _, _, code := c.resolveLogging(lf, "cli.plugin"); code >= 0 {
		return code
	}
	path := filepath.Join(c.forgeHome, "logs", "plugins", name+".log")
	data, err := os.ReadFile(path)
	if err != nil {
		return c.fail("plugin logs", err)
	}
	lines := strings.Split(strings.TrimRight(string(data), "\n"), "\n")
	if len(lines) > *n {
		lines = lines[len(lines)-*n:]
	}
	for _, l := range lines {
		fmt.Fprintln(c.stdout, l)
	}
	if !*follow {
		return 0
	}
	if err := tailFile(ctx, c.stdout, path, int64(len(data))); err != nil {
		return c.fail("plugin logs", err)
	}
	return 0
}

func runPluginStatus(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("plugin status")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.plugin")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("plugin status", err)
	}
	var rows []pluginRow
	if err := cl.do(ctx, http.MethodGet, "/api/v1/plugins", nil, &rows); err != nil {
		return c.fail("plugin status", err)
	}
	if *asJSON {
		c.printJSON(rows)
		return 0
	}
	tw := tabwriter.NewWriter(c.stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "NAME\tENABLED\tRUNNING\tPID\tRESTARTS\tCURSOR\tLAST EXIT")
	for _, r := range rows {
		enabled, running, pid := "no", "no", "-"
		if r.Enabled {
			enabled = "yes"
		}
		if r.Running {
			running, pid = "yes", fmt.Sprint(r.PID)
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%d\t%d\t%s\n", r.Name, enabled, running, pid, r.Restarts, r.Cursor, r.LastExit)
	}
	if err := tw.Flush(); err != nil {
		return c.fail("plugin status", err)
	}
	return 0
}

// findRepoPluginDir resolves the first-party install source: plugins/NAME
// with a plugin.toml in the checkout the running binary was built in, found
// by walking up from the executable.
func findRepoPluginDir(name string) (string, error) {
	exe, err := os.Executable()
	if err != nil {
		return "", fmt.Errorf("resolve forge binary: %w", err)
	}
	return findRepoPluginDirFrom(filepath.Dir(exe), name)
}

// findRepoPluginDirFrom is the walk, split out for tests.
func findRepoPluginDirFrom(start, name string) (string, error) {
	dir := start
	for {
		candidate := filepath.Join(dir, "plugins", name)
		if _, err := os.Stat(filepath.Join(candidate, "plugin.toml")); err == nil {
			return candidate, nil
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", fmt.Errorf("plugin %s: no plugins/%s/plugin.toml found above %s — first-party installs need a forge binary built in its checkout", name, name, start)
		}
		dir = parent
	}
}

// findRepoPluginsRoot locates the checkout's plugins/ directory for `forge
// plugin list`'s "available" rows.
func findRepoPluginsRoot() (string, error) {
	exe, err := os.Executable()
	if err != nil {
		return "", err
	}
	dir := filepath.Dir(exe)
	for {
		candidate := filepath.Join(dir, "plugins")
		if fi, err := os.Stat(candidate); err == nil && fi.IsDir() {
			if _, err := os.Stat(filepath.Join(dir, "go.mod")); err == nil {
				return candidate, nil
			}
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", fmt.Errorf("no plugins directory above %s", filepath.Dir(exe))
		}
		dir = parent
	}
}
