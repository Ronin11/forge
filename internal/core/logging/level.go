// Package logging is Forge's one logging mechanism: log/slog, a trace level below
// debug, per-component levels, correlation attributes carried in context, and two
// sinks (stderr under the operator's control, a JSON file at debug for the daemon
// and worker). Every package obtains its logger with For(component); nothing logs
// through the slog default logger.
package logging

import (
	"fmt"
	"log/slog"
	"maps"
	"sort"
	"strings"
	"sync"
)

// LevelTrace is below slog.LevelDebug and is reserved for firehoses: SQL
// statements, HTTP bodies, raw executor lines. It exists so debug stays readable.
const LevelTrace = slog.Level(-8)

// levelName is one entry of the operator vocabulary.
type levelName struct {
	name  string
	level slog.Level
}

// levelNames returns the vocabulary in ascending order. A function rather than a
// package variable so nothing can mutate the table.
func levelNames() []levelName {
	return []levelName{
		{"trace", LevelTrace},
		{"debug", slog.LevelDebug},
		{"info", slog.LevelInfo},
		{"warn", slog.LevelWarn},
		{"error", slog.LevelError},
	}
}

// ParseLevel is the single reader of level names, so flags, env, config, the API,
// and forwarded child records all accept exactly trace|debug|info|warn|error.
func ParseLevel(s string) (slog.Level, error) {
	s = strings.ToLower(strings.TrimSpace(s))
	for _, n := range levelNames() {
		if n.name == s {
			return n.level, nil
		}
	}
	return 0, fmt.Errorf("unknown log level %q (want trace|debug|info|warn|error)", s)
}

// LevelName renders a level the way ParseLevel reads it, so log output, flags, and
// the levels a child inherits agree ("TRACE" rather than slog's "DEBUG-4"). A
// level outside the vocabulary is rounded down to the nearest named one so a
// rendered spec always parses again.
func LevelName(l slog.Level) string {
	names := levelNames()
	best := names[0]
	for _, n := range names {
		if n.level <= l {
			best = n
		}
	}
	return strings.ToUpper(best.name)
}

// Levels is a default level plus per-component overrides. Components are dotted
// names ("worker.git"); an override for "worker" applies to "worker.git" unless a
// more specific one exists. Levels is a value; the store copies its map on every
// read and write so callers can never share (or race on) the underlying map.
type Levels struct {
	Default    slog.Level
	Components map[string]slog.Level
}

// ParseLevels reads the --log-level grammar: "info", "debug,store=trace", or
// "store=trace,worker=debug" (the default stays at base when no bare level is
// given). A spec is whole: whichever layer wins precedence replaces the others,
// it does not merge with them.
func ParseLevels(spec string, base slog.Level) (Levels, error) {
	out := Levels{Default: base, Components: map[string]slog.Level{}}
	for _, part := range strings.Split(spec, ",") {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		name, value, isComponent := strings.Cut(part, "=")
		if !isComponent {
			l, err := ParseLevel(name)
			if err != nil {
				return Levels{}, err
			}
			out.Default = l
			continue
		}
		name = strings.TrimSpace(name)
		if name == "" {
			return Levels{}, fmt.Errorf("log level %q: empty component name", part)
		}
		l, err := ParseLevel(value)
		if err != nil {
			return Levels{}, fmt.Errorf("log level for %s: %w", name, err)
		}
		out.Components[name] = l
	}
	return out, nil
}

// For returns the effective level for a component by longest dotted prefix.
func (l Levels) For(component string) slog.Level {
	for name := component; name != ""; {
		if lvl, ok := l.Components[name]; ok {
			return lvl
		}
		i := strings.LastIndex(name, ".")
		if i < 0 {
			break
		}
		name = name[:i]
	}
	return l.Default
}

// String renders in the same grammar ParseLevels reads, components sorted, so a
// child started with this value sees exactly the parent's levels.
func (l Levels) String() string {
	parts := []string{strings.ToLower(LevelName(l.Default))}
	names := make([]string, 0, len(l.Components))
	for name := range l.Components {
		names = append(names, name)
	}
	sort.Strings(names)
	for _, name := range names {
		parts = append(parts, name+"="+strings.ToLower(LevelName(l.Components[name])))
	}
	return strings.Join(parts, ",")
}

func (l Levels) clone() Levels {
	return Levels{Default: l.Default, Components: maps.Clone(l.Components)}
}

// levelStore is the mutable cell behind runtime level changes (API call, SIGUSR1).
// It is the only mutable state in the package.
type levelStore struct {
	mu       sync.RWMutex // guards current and previous
	current  Levels
	previous *Levels // set while debug was toggled on, to toggle back
}

func (s *levelStore) get() Levels {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.current.clone()
}

func (s *levelStore) set(l Levels) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.current = l.clone()
	s.previous = nil
}

// toggleDebug makes the default level at least debug (a default already at trace
// stays at trace), or restores what it was before the last toggle. Component
// overrides are untouched: SIGUSR1 answers "show me more", not "reset my config".
// An explicit set in between clears the memory, so the toggle never restores a
// level the operator has since replaced.
func (s *levelStore) toggleDebug() Levels {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.previous != nil {
		s.current = *s.previous
		s.previous = nil
		return s.current.clone()
	}
	prev := s.current.clone()
	s.previous = &prev
	s.current = s.current.clone()
	if s.current.Default > slog.LevelDebug {
		s.current.Default = slog.LevelDebug
	}
	return s.current.clone()
}
