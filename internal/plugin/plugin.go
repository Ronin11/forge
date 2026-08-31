// Package plugin is the one home for the plugin contract (DESIGN.md §17):
// the manifest shape, the capability and scope vocabularies, and manifest
// discovery. The daemon supervises processes and mints tokens; this package
// only says what a valid plugin is.
package plugin

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"

	"github.com/BurntSushi/toml"

	"forge/internal/model"
)

// Capabilities a plugin may declare (DESIGN.md §17).
const (
	CapEvents   = "events"   // consumes the journal SSE stream
	CapTools    = "tools"    // is an MCP server on its stdio
	CapIntake   = "intake"   // creates Work via the API
	CapAnnotate = "annotate" // writes external_refs
)

// Scopes a plugin token may carry. A request outside the token's scopes is
// 403 and journaled.
const (
	ScopeEventsRead    = "events:read"
	ScopeWorkRead      = "work:read"
	ScopeWorkWrite     = "work:write"
	ScopeUsageRead     = "usage:read"
	ScopeKbRead        = "kb:read"
	ScopeKbWrite       = "kb:write"
	ScopeProposalRead  = "proposal:read"
	ScopeToolsProvide  = "tools:provide"
	ScopeAnnotateWrite = "annotate:write"
)

var validCapabilities = map[string]bool{CapEvents: true, CapTools: true, CapIntake: true, CapAnnotate: true}

var validScopes = map[string]bool{
	ScopeEventsRead: true, ScopeWorkRead: true, ScopeWorkWrite: true, ScopeUsageRead: true,
	ScopeKbRead: true, ScopeKbWrite: true, ScopeProposalRead: true, ScopeToolsProvide: true, ScopeAnnotateWrite: true,
}

// ValidScope reports whether s is a known scope.
func ValidScope(s string) bool { return validScopes[s] }

// Manifest is plugin.toml. Command is argv relative to the plugin directory
// (any language); Build, when present, is run once at install in the plugin
// directory (first-party Go plugins compile themselves).
type Manifest struct {
	Name         string   `toml:"name"`
	Version      string   `toml:"version"`
	Description  string   `toml:"description"`
	Command      []string `toml:"command"`
	Capabilities []string `toml:"capabilities"`
	Scopes       []string `toml:"scopes"`
	Restart      string   `toml:"restart"` // always | on-failure | never
	Build        []string `toml:"build,omitempty"`

	// Dir is where the manifest was read from; not part of the file.
	Dir string `toml:"-"`
}

// Validate enforces the contract; the daemon refuses invalid manifests.
func (m *Manifest) Validate() error {
	if err := model.ValidateName(m.Name); err != nil {
		return fmt.Errorf("plugin name: %w", err)
	}
	if m.Version == "" {
		return fmt.Errorf("plugin %s: version is required", m.Name)
	}
	if len(m.Command) == 0 {
		return fmt.Errorf("plugin %s: command is required", m.Name)
	}
	if m.Restart == "" {
		m.Restart = "on-failure"
	}
	switch m.Restart {
	case "always", "on-failure", "never":
	default:
		return fmt.Errorf("plugin %s: restart %q: want always, on-failure, or never", m.Name, m.Restart)
	}
	if len(m.Capabilities) == 0 {
		return fmt.Errorf("plugin %s: at least one capability is required", m.Name)
	}
	for _, c := range m.Capabilities {
		if !validCapabilities[c] {
			return fmt.Errorf("plugin %s: capability %q: want events, tools, intake, or annotate", m.Name, c)
		}
	}
	for _, s := range m.Scopes {
		if !validScopes[s] {
			return fmt.Errorf("plugin %s: scope %q is not known", m.Name, s)
		}
	}
	if hasCap(m.Capabilities, CapTools) && !hasScope(m.Scopes, ScopeToolsProvide) {
		return fmt.Errorf("plugin %s: the tools capability requires the tools:provide scope", m.Name)
	}
	return nil
}

func hasCap(caps []string, c string) bool {
	for _, x := range caps {
		if x == c {
			return true
		}
	}
	return false
}

func hasScope(scopes []string, s string) bool {
	for _, x := range scopes {
		if x == s {
			return true
		}
	}
	return false
}

// Has reports whether the manifest declares capability c.
func (m *Manifest) Has(c string) bool { return hasCap(m.Capabilities, c) }

// HasScope reports whether the manifest declares scope s.
func (m *Manifest) HasScope(s string) bool { return hasScope(m.Scopes, s) }

// Load reads and validates one plugin.toml. dir is the plugin directory.
func Load(dir string) (*Manifest, error) {
	var m Manifest
	path := filepath.Join(dir, "plugin.toml")
	if _, err := toml.DecodeFile(path, &m); err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	m.Dir = dir
	if err := m.Validate(); err != nil {
		return nil, err
	}
	if base := filepath.Base(dir); base != m.Name {
		return nil, fmt.Errorf("plugin %s: directory %s must match the manifest name", m.Name, base)
	}
	return &m, nil
}

// LoadFromRoots loads the named plugin from the first root that holds it,
// searching in Discover's order so the same earlier-root-wins rule decides
// which copy a caller resolving one plugin by name gets.
func LoadFromRoots(roots []string, name string) (*Manifest, error) {
	for _, root := range roots {
		dir := filepath.Join(root, name)
		if _, err := os.Stat(filepath.Join(dir, "plugin.toml")); err == nil {
			return Load(dir)
		}
	}
	return nil, fmt.Errorf("plugin %s: no plugin.toml under any discovery root", name)
}

// Discover loads every valid manifest under the given roots (first-party
// repo plugins, then <home>/plugins), sorted by name; an invalid manifest is
// reported through onErr and skipped, never fatal.
func Discover(roots []string, onErr func(dir string, err error)) []*Manifest {
	byName := map[string]*Manifest{}
	for _, root := range roots {
		entries, err := os.ReadDir(root)
		if err != nil {
			continue // a missing root is normal
		}
		for _, e := range entries {
			if !e.IsDir() {
				continue
			}
			dir := filepath.Join(root, e.Name())
			if _, serr := os.Stat(filepath.Join(dir, "plugin.toml")); serr != nil {
				// Not a plugin (plugins/examples/, scratch dirs): silently
				// skipped — only an invalid manifest is worth reporting.
				continue
			}
			m, err := Load(dir)
			if err != nil {
				if onErr != nil {
					onErr(dir, err)
				}
				continue
			}
			if _, dup := byName[m.Name]; dup {
				if onErr != nil {
					onErr(dir, fmt.Errorf("plugin %s: already discovered; later roots do not shadow earlier ones", m.Name))
				}
				continue
			}
			byName[m.Name] = m
		}
	}
	names := make([]string, 0, len(byName))
	for n := range byName {
		names = append(names, n)
	}
	sort.Strings(names)
	out := make([]*Manifest, 0, len(names))
	for _, n := range names {
		out = append(out, byName[n])
	}
	return out
}
