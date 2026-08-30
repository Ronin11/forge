package worker

import (
	"errors"
	"fmt"
	"strconv"
	"strings"

	"forge/internal/protocol"
)

// ExecutorConfig is one [executors.<name>] table in worker.toml.
type ExecutorConfig struct {
	Command          []string `toml:"command"`
	Output           string   `toml:"output"`
	AllowedToolsFlag string   `toml:"allowed_tools_flag"` // capability: pass routine allowed_tools with this flag
}

// Validate checks the template and the parser name.
func (e ExecutorConfig) Validate(name string) error {
	if len(e.Command) == 0 {
		return fmt.Errorf("executor %q: command is empty", name)
	}
	if e.Output == "" {
		return fmt.Errorf("executor %q: output parser is required", name)
	}
	if _, err := NewParser(e.Output); err != nil {
		return fmt.Errorf("executor %q: %w", name, err)
	}
	for _, arg := range e.Command {
		for _, v := range templateVars(arg) {
			switch v {
			case "model", "max_turns", "repo", "worktree":
			default:
				return fmt.Errorf("executor %q: unknown template variable {{%s}}", name, v)
			}
		}
	}
	return nil
}

func templateVars(arg string) []string {
	var out []string
	for {
		start := strings.Index(arg, "{{")
		if start < 0 {
			return out
		}
		end := strings.Index(arg[start:], "}}")
		if end < 0 {
			return out
		}
		out = append(out, strings.TrimSpace(arg[start+2:start+end]))
		arg = arg[start+end+2:]
	}
}

// Render expands the command template for one attempt.
func (e ExecutorConfig) Render(s protocol.Settings, repo, worktree string) ([]string, error) {
	if s.Model == "" {
		return nil, errors.New("model is required")
	}
	vars := map[string]string{
		"model":     s.Model,
		"max_turns": strconv.Itoa(s.MaxTurns),
		"repo":      repo,
		"worktree":  worktree,
	}
	out := make([]string, 0, len(e.Command)+2)
	for _, arg := range e.Command {
		for k, v := range vars {
			arg = strings.ReplaceAll(arg, "{{"+k+"}}", v)
		}
		out = append(out, arg)
	}
	if len(s.AllowedTools) > 0 && e.AllowedToolsFlag != "" {
		out = append(out, e.AllowedToolsFlag, strings.Join(s.AllowedTools, ","))
	}
	return out, nil
}
