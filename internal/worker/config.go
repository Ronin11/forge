// Package worker executes attempts: it validates registered checkouts, prepares
// worktrees, launches executors under supervision, streams events, verifies,
// cleans up, and reconciles after crashes. It never imports the control plane or
// the store; everything it knows about the daemon arrives over the API.
package worker

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/BurntSushi/toml"

	"forge/internal/logging"
	"forge/internal/model"
)

// Config is ~/.forge/worker.toml after defaults and validation. Nothing else in
// the package reads a raw map.
type Config struct {
	Daemon        string `toml:"daemon"`     // unix://<path> or http://127.0.0.1:7340
	TokenFile     string `toml:"token_file"` // required for http://, ignored for unix://
	Name          string `toml:"name"`
	MaxConcurrent int    `toml:"max_concurrent"`
	DataDir       string `toml:"data_dir"`

	Executors    map[string]ExecutorConfig   `toml:"executors"`
	Repositories map[string]RepositoryConfig `toml:"repositories"`
	Greenfield   GreenfieldConfig            `toml:"greenfield"`
	Log          logging.Config              `toml:"log"`

	// path is where the file was read from; relative paths resolve against it.
	path string
}

// ExecutorConfig is one [executors.<name>] entry (DESIGN.md §7.4).
type ExecutorConfig struct {
	Command      []string `toml:"command"`
	Output       string   `toml:"output"`
	Capabilities []string `toml:"capabilities"`
	Sandbox      *bool    `toml:"sandbox"` // nil → true
}

// RepositoryConfig is one [repositories.<name>] entry.
type RepositoryConfig struct {
	Path       string `toml:"path"`
	BaseBranch string `toml:"base_branch"`
	Project    string `toml:"project"`
}

// GreenfieldConfig is where greenfield projects land (design pending, MODES.md).
type GreenfieldConfig struct {
	ProjectsRoot string `toml:"projects_root"`
}

// DefaultConfig is what bootstrap writes when worker.toml is absent; forgeHome
// is expanded into the paths.
func DefaultConfig(forgeHome, forgeBinary string) Config {
	return Config{
		Daemon:        "unix://" + filepath.Join(forgeHome, "forge.sock"),
		TokenFile:     filepath.Join(forgeHome, "token"),
		Name:          "local",
		MaxConcurrent: 4,
		DataDir:       filepath.Join(forgeHome, "worker"),
		Executors: map[string]ExecutorConfig{
			"claude-code": {
				Command: []string{"claude", "--print", "--verbose", "--output-format", "stream-json",
					"--dangerously-skip-permissions", "--strict-mcp-config",
					"--model", "{{model}}", "--max-turns", "{{max_turns}}", "--mcp-config", "{{mcp_config}}"},
				Output:       "claude-stream-json",
				Capabilities: []string{"allowed_tools", "builtin_tools", "json_schema", "resume", "max_budget_usd", "effort", "append_system_prompt"},
			},
			"fake-claude": {
				Command:      []string{forgeBinary, "fake-claude", "--fixture", "{{fixture}}", "--model", "{{model}}", "--max-turns", "{{max_turns}}", "--mcp-config", "{{mcp_config}}"},
				Output:       "claude-stream-json",
				Capabilities: []string{"allowed_tools", "json_schema", "resume"},
			},
		},
		Repositories: map[string]RepositoryConfig{},
	}
}

// LoadConfig reads and validates worker.toml. Unknown keys are errors: a typo in
// a config file should fail loudly, not silently do nothing.
func LoadConfig(path string) (*Config, error) {
	var c Config
	meta, err := toml.DecodeFile(path, &c)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	if undecoded := meta.Undecoded(); len(undecoded) > 0 {
		keys := make([]string, len(undecoded))
		for i, k := range undecoded {
			keys[i] = k.String()
		}
		return nil, fmt.Errorf("%s: unknown keys: %s", path, strings.Join(keys, ", "))
	}
	c.path = path
	if err := c.finish(); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	return &c, nil
}

// finish applies defaults, expands ~ and relative paths, and validates.
func (c *Config) finish() error {
	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("resolve home: %w", err)
	}
	base := filepath.Dir(c.path)
	expand := func(p string) string {
		if p == "" {
			return p
		}
		if strings.HasPrefix(p, "~/") {
			p = filepath.Join(home, p[2:])
		}
		if !filepath.IsAbs(p) {
			p = filepath.Join(base, p)
		}
		return filepath.Clean(p)
	}
	if c.Name == "" {
		c.Name = "local"
	}
	if err := model.ValidateName(c.Name); err != nil {
		return fmt.Errorf("name: %w", err)
	}
	if c.MaxConcurrent == 0 {
		c.MaxConcurrent = 4
	}
	if c.MaxConcurrent < 1 || c.MaxConcurrent > 100 {
		return fmt.Errorf("max_concurrent must be in 1..100")
	}
	if c.DataDir == "" {
		c.DataDir = filepath.Join(base, "worker")
	}
	c.DataDir = expand(c.DataDir)
	c.TokenFile = expand(c.TokenFile)
	c.Greenfield.ProjectsRoot = expand(c.Greenfield.ProjectsRoot)
	switch {
	case strings.HasPrefix(c.Daemon, "unix://"):
		c.Daemon = "unix://" + expand(strings.TrimPrefix(c.Daemon, "unix://"))
	case strings.HasPrefix(c.Daemon, "http://"):
		if c.TokenFile == "" {
			return fmt.Errorf("token_file is required with an http:// daemon")
		}
	case c.Daemon == "":
		return fmt.Errorf("daemon is required (unix://<socket> or http://host:port)")
	default:
		return fmt.Errorf("daemon %q: want unix:// or http://", c.Daemon)
	}
	if len(c.Executors) == 0 {
		return fmt.Errorf("at least one [executors.<name>] is required")
	}
	for name, e := range c.Executors {
		if err := model.ValidateName(name); err != nil {
			return fmt.Errorf("executor: %w", err)
		}
		if len(e.Command) == 0 {
			return fmt.Errorf("executor %s: command is required", name)
		}
		if e.Output == "" {
			return fmt.Errorf("executor %s: output parser is required", name)
		}
	}
	for name, r := range c.Repositories {
		if err := model.ValidateName(name); err != nil {
			return fmt.Errorf("repository: %w", err)
		}
		if r.Path == "" {
			return fmt.Errorf("repository %s: path is required", name)
		}
		r.Path = expand(r.Path)
		if r.Project == "" {
			r.Project = "default"
		}
		if err := model.ValidateName(r.Project); err != nil {
			return fmt.Errorf("repository %s: project: %w", name, err)
		}
		c.Repositories[name] = r
	}
	if err := c.Log.Validate(); err != nil {
		return err
	}
	return nil
}

// Path is the file the config was loaded from.
func (c *Config) Path() string { return c.path }

// RepositoryNames returns the configured names, sorted, so registration and
// validation are deterministic.
func (c *Config) RepositoryNames() []string {
	names := make([]string, 0, len(c.Repositories))
	for n := range c.Repositories {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}

// SandboxEnabled reports the executor's sandbox flag (default true).
func (e ExecutorConfig) SandboxEnabled() bool { return e.Sandbox == nil || *e.Sandbox }

// WriteDefault writes DefaultConfig to path if absent (bootstrap); it never
// overwrites an operator's file.
func WriteDefault(path, forgeHome, forgeBinary string) (written bool, err error) {
	if _, err := os.Stat(path); err == nil {
		return false, nil
	} else if !os.IsNotExist(err) {
		return false, fmt.Errorf("stat %s: %w", path, err)
	}
	var b strings.Builder
	if err := toml.NewEncoder(&b).Encode(DefaultConfig(forgeHome, forgeBinary)); err != nil {
		return false, fmt.Errorf("encode default worker config: %w", err)
	}
	header := "# Forge worker configuration. Written by bootstrap on " + time.Now().UTC().Format(time.RFC3339) + "; edit freely.\n" +
		"# Register checkouts under [repositories.<name>] with path and optional base_branch.\n\n"
	if err := os.WriteFile(path, []byte(header+b.String()), 0o600); err != nil {
		return false, fmt.Errorf("write %s: %w", path, err)
	}
	return true, nil
}

// AddRepository appends [repositories.<name>] to worker.toml (DESIGN §1.3
// "repositories on the fly": the daemon owns every file bootstrap writes; the
// worker re-reads the file on its next registration tick). An existing entry
// is an error — on-the-fly registration never rewrites what a human set.
func AddRepository(cfgPath, name, repoPath string) error {
	if err := model.ValidateName(name); err != nil {
		return err
	}
	m := map[string]any{}
	if _, err := toml.DecodeFile(cfgPath, &m); err != nil {
		return fmt.Errorf("read %s: %w", cfgPath, err)
	}
	repos, ok := m["repositories"].(map[string]any)
	if !ok || repos == nil {
		repos = map[string]any{}
	}
	if _, exists := repos[name]; exists {
		return fmt.Errorf("repository %s already in %s", name, cfgPath)
	}
	repos[name] = map[string]any{"path": repoPath}
	m["repositories"] = repos
	var b strings.Builder
	if err := toml.NewEncoder(&b).Encode(m); err != nil {
		return fmt.Errorf("encode %s: %w", cfgPath, err)
	}
	if err := os.WriteFile(cfgPath, []byte(b.String()), 0o600); err != nil {
		return fmt.Errorf("write %s: %w", cfgPath, err)
	}
	return nil
}
