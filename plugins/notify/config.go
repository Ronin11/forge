// config.go reads the optional per-kind toggles from
// <FORGE_PLUGIN_DIR>/notify.toml: a [notify] table with questions, failures,
// throttling, and proposals booleans. Everything defaults to on; an absent
// file or an absent key changes nothing.
package main

import (
	"fmt"
	"os"

	"github.com/BurntSushi/toml"
)

// config is which notification kinds are enabled.
type config struct {
	Questions  bool
	Failures   bool
	Throttling bool
	Proposals  bool
}

func defaultConfig() config {
	return config{Questions: true, Failures: true, Throttling: true, Proposals: true}
}

// loadConfig reads path, tolerating its absence; keys are pointers so an
// absent key keeps its default rather than reading as false.
func loadConfig(path string) (config, error) {
	cfg := defaultConfig()
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
		Notify struct {
			Questions  *bool `toml:"questions"`
			Failures   *bool `toml:"failures"`
			Throttling *bool `toml:"throttling"`
			Proposals  *bool `toml:"proposals"`
		} `toml:"notify"`
	}
	if err := toml.Unmarshal(b, &file); err != nil {
		return cfg, fmt.Errorf("parse %s: %w", path, err)
	}
	if v := file.Notify.Questions; v != nil {
		cfg.Questions = *v
	}
	if v := file.Notify.Failures; v != nil {
		cfg.Failures = *v
	}
	if v := file.Notify.Throttling; v != nil {
		cfg.Throttling = *v
	}
	if v := file.Notify.Proposals; v != nil {
		cfg.Proposals = *v
	}
	return cfg, nil
}
