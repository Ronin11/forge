// config.go reads <FORGE_PLUGIN_DIR>/signal.toml: the Signal account Forge
// sends from, the recipient it notifies and takes commands from, the signal-cli
// binary, and per-kind outbound toggles. account and recipient are required to
// do anything; everything else has a default.
package main

import (
	"fmt"
	"os"

	"github.com/BurntSushi/toml"
)

// config is the plugin's settings.
type config struct {
	Account     string // the Signal number Forge sends from, e.g. "+15551234567"
	Recipient   string // the number/username notified, and the only sender whose commands are honored
	SignalCLI   string // signal-cli binary (path or name on PATH)
	PollSeconds int    // inbound receive cadence
	UI          string // base UI URL put in notification bodies
	DefaultRepo string // repo for a bare "task …" command with no repo
	// Outbound toggles (all default on).
	Questions  bool
	Failures   bool
	Proposals  bool
	Throttling bool
	// Intake enables inbound commands (task/answer/status); default on.
	Intake bool
}

func defaultConfig() config {
	return config{
		SignalCLI: "signal-cli", PollSeconds: 10, UI: defaultUI,
		Questions: true, Failures: true, Proposals: true, Throttling: true, Intake: true,
	}
}

// loadConfig reads path, tolerating its absence. Pointer fields distinguish an
// absent key (keep the default) from an explicit false.
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
		Signal struct {
			Account     string `toml:"account"`
			Recipient   string `toml:"recipient"`
			SignalCLI   string `toml:"signal_cli"`
			PollSeconds int    `toml:"poll_seconds"`
			UI          string `toml:"ui"`
			DefaultRepo string `toml:"default_repo"`
			Questions   *bool  `toml:"questions"`
			Failures    *bool  `toml:"failures"`
			Proposals   *bool  `toml:"proposals"`
			Throttling  *bool  `toml:"throttling"`
			Intake      *bool  `toml:"intake"`
		} `toml:"signal"`
	}
	if err := toml.Unmarshal(b, &file); err != nil {
		return cfg, fmt.Errorf("parse %s: %w", path, err)
	}
	sig := file.Signal
	cfg.Account, cfg.Recipient, cfg.DefaultRepo = sig.Account, sig.Recipient, sig.DefaultRepo
	if sig.SignalCLI != "" {
		cfg.SignalCLI = sig.SignalCLI
	}
	if sig.PollSeconds > 0 {
		cfg.PollSeconds = sig.PollSeconds
	}
	if sig.UI != "" {
		cfg.UI = sig.UI
	}
	for dst, src := range map[*bool]*bool{
		&cfg.Questions: sig.Questions, &cfg.Failures: sig.Failures,
		&cfg.Proposals: sig.Proposals, &cfg.Throttling: sig.Throttling, &cfg.Intake: sig.Intake,
	} {
		if src != nil {
			*dst = *src
		}
	}
	return cfg, nil
}
