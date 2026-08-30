package worker

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

// Config is ~/.forge/worker.toml.
type Config struct {
	Server        string                    `toml:"server"`
	Name          string                    `toml:"name"`
	TokenFile     string                    `toml:"token_file"`
	MaxConcurrent int                       `toml:"max_concurrent"`
	DataDir       string                    `toml:"data_dir"`
	Executors     map[string]ExecutorConfig `toml:"executors"`
	Repositories  map[string]RepoConfig     `toml:"repositories"`
}

// RepoConfig is one [repositories.<name>] table.
type RepoConfig struct {
	Path       string `toml:"path"`
	BaseBranch string `toml:"base_branch"`
}

// LoadConfig reads and validates the worker configuration file.
func LoadConfig(path string) (Config, error) {
	body, err := os.ReadFile(path)
	if err != nil {
		return Config{}, err
	}
	var c Config
	d := toml.NewDecoder(strings.NewReader(string(body)))
	d.DisallowUnknownFields()
	if err := d.Decode(&c); err != nil {
		return Config{}, fmt.Errorf("%s: %w", path, err)
	}
	if c.Server == "" {
		c.Server = "http://127.0.0.1:7340"
	}
	if c.Name == "" {
		c.Name, _ = os.Hostname()
		if c.Name == "" {
			c.Name = "worker"
		}
	}
	if c.MaxConcurrent == 0 {
		c.MaxConcurrent = 4
	}
	if c.MaxConcurrent < 1 || c.MaxConcurrent > 64 {
		return Config{}, errors.New("max_concurrent must be between 1 and 64")
	}
	c.TokenFile = expandHome(c.TokenFile, "~/.forge/worker.token")
	c.DataDir = expandHome(c.DataDir, "~/.forge/worker")
	if len(c.Executors) == 0 {
		return Config{}, errors.New("at least one [executors.<name>] is required")
	}
	for name, e := range c.Executors {
		if err := e.Validate(name); err != nil {
			return Config{}, err
		}
	}
	if len(c.Repositories) == 0 {
		return Config{}, errors.New("at least one [repositories.<name>] is required")
	}
	for name, r := range c.Repositories {
		if r.Path == "" {
			return Config{}, fmt.Errorf("repository %q: path is required", name)
		}
		if strings.ContainsAny(name, "/ \t") {
			return Config{}, fmt.Errorf("repository name %q must not contain slashes or spaces", name)
		}
	}
	return c, nil
}

func expandHome(p, def string) string {
	if p == "" {
		p = def
	}
	if strings.HasPrefix(p, "~/") {
		home, err := os.UserHomeDir()
		if err == nil {
			p = filepath.Join(home, p[2:])
		}
	}
	return p
}

// ValidateRepositories resolves every configured repository in name order.
func (c Config) ValidateRepositories(ctx context.Context) ([]Repository, error) {
	names := make([]string, 0, len(c.Repositories))
	for n := range c.Repositories {
		names = append(names, n)
	}
	sort.Strings(names)
	var out []Repository
	seen := map[string]string{}
	for _, n := range names {
		rc := c.Repositories[n]
		r, err := ValidateRepository(ctx, n, expandHome(rc.Path, rc.Path), rc.BaseBranch)
		if err != nil {
			return nil, err
		}
		if prev, dup := seen[r.Path]; dup {
			return nil, fmt.Errorf("repositories %q and %q point at the same checkout", prev, n)
		}
		seen[r.Path] = n
		out = append(out, r)
	}
	return out, nil
}

// ExecutorNames lists configured executors, sorted.
func (c Config) ExecutorNames() []string {
	names := make([]string, 0, len(c.Executors))
	for n := range c.Executors {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}

// ReadToken reads the shared secret file.
func ReadToken(path string) (string, error) {
	body, err := os.ReadFile(path)
	if err != nil {
		return "", fmt.Errorf("read worker token: %w", err)
	}
	token := strings.TrimSpace(string(body))
	if token == "" {
		return "", errors.New("worker token file is empty")
	}
	return token, nil
}
