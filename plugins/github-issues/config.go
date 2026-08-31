// config.go reads <FORGE_PLUGIN_DIR>/github-issues.toml: the poll cadence and
// one [[repo]] table per GitHub repository to watch. An absent file parses to
// a config with no repos, so the plugin idles rather than crashing. Defaults
// are applied and every repo is validated here, so the poll loop never sees a
// raw map (STYLE §3).
package main

import (
	"fmt"
	"os"
	"regexp"

	"github.com/BurntSushi/toml"
)

// minPollSeconds is the floor for poll_seconds; a tighter cadence only hammers
// the GitHub API without picking up issues faster.
const minPollSeconds = 15

// defaultPollSeconds is used when poll_seconds is absent.
const defaultPollSeconds = 60

// repoName matches an owner/name GitHub slug — exactly one slash, no spaces.
var repoName = regexp.MustCompile(`^[^/\s]+/[^/\s]+$`)

// config is the parsed, validated plugin configuration.
type config struct {
	PollSeconds int
	Repos       []repoConfig
}

// repoConfig is one watched repository: where the issues live (GitHub), which
// registered Forge repository the task runs against (Forge), and the task
// shape to create.
type repoConfig struct {
	GitHub    string // owner/name
	Forge     string // registered Forge repository name
	Label     string // only issues with this label; "" = every open issue
	Mode      string
	Model     string
	Class     string
	Autonomy  string
	Integrate bool
	Comment   bool
	DoneLabel string // added when the task finishes; "" = none
}

// loadConfig reads path, tolerating its absence (an empty config, so the
// plugin idles). Defaults are applied and each repo validated; unknown keys
// are ignored, matching the other first-party plugins (notify, status-file).
func loadConfig(path string) (config, error) {
	cfg := config{PollSeconds: defaultPollSeconds}
	if path == "" {
		return cfg, nil
	}
	b, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return cfg, nil
	}
	if err != nil {
		return cfg, fmt.Errorf("read %s: %w", path, err)
	}
	var file struct {
		PollSeconds *int `toml:"poll_seconds"`
		Repo        []struct {
			GitHub    string `toml:"github"`
			Forge     string `toml:"forge"`
			Label     string `toml:"label"`
			Mode      string `toml:"mode"`
			Model     string `toml:"model"`
			Class     string `toml:"class"`
			Autonomy  string `toml:"autonomy"`
			Integrate *bool  `toml:"integrate"`
			Comment   *bool  `toml:"comment"`
			DoneLabel string `toml:"done_label"`
		} `toml:"repo"`
	}
	if err := toml.Unmarshal(b, &file); err != nil {
		return cfg, fmt.Errorf("parse %s: %w", path, err)
	}
	if file.PollSeconds != nil {
		cfg.PollSeconds = *file.PollSeconds
	}
	if cfg.PollSeconds < minPollSeconds {
		cfg.PollSeconds = minPollSeconds
	}
	for i, r := range file.Repo {
		if !repoName.MatchString(r.GitHub) {
			return cfg, fmt.Errorf("%s: [[repo]] %d: github %q must be owner/name", path, i, r.GitHub)
		}
		if r.Forge == "" {
			return cfg, fmt.Errorf("%s: [[repo]] %d (%s): forge is required", path, i, r.GitHub)
		}
		rc := repoConfig{
			GitHub:    r.GitHub,
			Forge:     r.Forge,
			Label:     r.Label,
			Mode:      orDefault(r.Mode, "implement"),
			Model:     r.Model,
			Class:     orDefault(r.Class, "normal"),
			Autonomy:  orDefault(r.Autonomy, "auto"),
			Integrate: r.Integrate != nil && *r.Integrate,
			Comment:   r.Comment == nil || *r.Comment,
			DoneLabel: r.DoneLabel,
		}
		cfg.Repos = append(cfg.Repos, rc)
	}
	return cfg, nil
}

// orDefault returns v, or def when v is empty.
func orDefault(v, def string) string {
	if v == "" {
		return def
	}
	return v
}
