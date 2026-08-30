package worker

import (
	"context"
	"fmt"
	"os/exec"
	"sort"
	"strconv"
	"strings"
)

// Executor turns a launch request into a command. One implementation ships (the
// template executor); a second is a new file plus a config entry.
type Executor interface {
	Command(ctx context.Context, req LaunchRequest) (*exec.Cmd, error)
	Capabilities() []string
	// OutputParser names the registered parser for this executor's stdout.
	OutputParser() string
}

// LaunchRequest is what the attempt runner knows when it launches.
type LaunchRequest struct {
	Model        string // resolved model id
	MaxTurns     int
	Repo         string
	Worktree     string
	MCPConfig    string
	SessionID    string // set on resume
	Fixture      string // fake-claude only
	AllowedTools []string
	NoBuiltins   bool
	JSONSchema   string
	MaxBudgetUSD float64
	Effort       string
	SystemAppend string
	Env          []string // the pass-through environment (DESIGN §1.1)
}

// Capabilities an executor may declare (DESIGN.md §7.4).
const (
	CapAllowedTools       = "allowed_tools"
	CapBuiltinTools       = "builtin_tools"
	CapJSONSchema         = "json_schema"
	CapResume             = "resume"
	CapMaxBudgetUSD       = "max_budget_usd"
	CapEffort             = "effort"
	CapAppendSystemPrompt = "append_system_prompt"
	CapSteer              = "steer"
)

// TemplateExecutor renders a command template from worker.toml and appends the
// flags for each capability it declares and the request uses.
type TemplateExecutor struct {
	name string
	cfg  ExecutorConfig
}

// NewTemplateExecutor validates the template once so launches cannot fail on a
// typo in a variable name.
func NewTemplateExecutor(name string, cfg ExecutorConfig) (*TemplateExecutor, error) {
	for _, arg := range cfg.Command {
		for _, v := range templateVars(arg) {
			if !knownVars[v] {
				return nil, fmt.Errorf("executor %s: unknown template variable {{%s}}", name, v)
			}
		}
	}
	for _, c := range cfg.Capabilities {
		if !knownCaps[c] {
			return nil, fmt.Errorf("executor %s: unknown capability %q", name, c)
		}
	}
	return &TemplateExecutor{name: name, cfg: cfg}, nil
}

var knownVars = map[string]bool{"model": true, "max_turns": true, "repo": true, "worktree": true, "mcp_config": true, "session_id": true, "fixture": true}

var knownCaps = map[string]bool{CapAllowedTools: true, CapBuiltinTools: true, CapJSONSchema: true, CapResume: true, CapMaxBudgetUSD: true, CapEffort: true, CapAppendSystemPrompt: true, CapSteer: true}

// Name is the config key.
func (e *TemplateExecutor) Name() string { return e.name }

// Capabilities are the declared ones, sorted.
func (e *TemplateExecutor) Capabilities() []string {
	out := append([]string(nil), e.cfg.Capabilities...)
	sort.Strings(out)
	return out
}

// OutputParser is the configured parser name.
func (e *TemplateExecutor) OutputParser() string { return e.cfg.Output }

// Has reports a declared capability.
func (e *TemplateExecutor) Has(cap string) bool {
	for _, c := range e.cfg.Capabilities {
		if c == cap {
			return true
		}
	}
	return false
}

// Command renders the template and appends capability flags. cwd is the
// worktree; the caller wires stdin/stdout/stderr and the sandbox wrapper.
func (e *TemplateExecutor) Command(ctx context.Context, req LaunchRequest) (*exec.Cmd, error) {
	if len(e.cfg.Command) == 0 {
		return nil, fmt.Errorf("executor %s: empty command", e.name)
	}
	vars := map[string]string{
		"model": req.Model, "max_turns": strconv.Itoa(req.MaxTurns), "repo": req.Repo, "worktree": req.Worktree,
		"mcp_config": req.MCPConfig, "session_id": req.SessionID, "fixture": req.Fixture,
	}
	args := make([]string, 0, len(e.cfg.Command)+16)
	for _, arg := range e.cfg.Command {
		args = append(args, render(arg, vars))
	}
	if e.Has(CapAllowedTools) && len(req.AllowedTools) > 0 {
		args = append(args, "--allowedTools", strings.Join(req.AllowedTools, ","))
	}
	if e.Has(CapBuiltinTools) && req.NoBuiltins {
		args = append(args, "--tools", "")
	}
	if e.Has(CapJSONSchema) && req.JSONSchema != "" {
		args = append(args, "--json-schema", req.JSONSchema)
	}
	if e.Has(CapResume) && req.SessionID != "" {
		args = append(args, "--resume", req.SessionID)
	}
	if e.Has(CapMaxBudgetUSD) && req.MaxBudgetUSD > 0 {
		args = append(args, "--max-budget-usd", strconv.FormatFloat(req.MaxBudgetUSD, 'f', -1, 64))
	}
	if e.Has(CapEffort) && req.Effort != "" {
		args = append(args, "--effort", req.Effort)
	}
	if e.Has(CapAppendSystemPrompt) && req.SystemAppend != "" {
		args = append(args, "--append-system-prompt", req.SystemAppend)
	}
	// exec.Command, not CommandContext: the supervisor owns the deadline and
	// kills the whole process group; CommandContext's Cancel would kill only
	// the direct child. ctx is kept in the signature for executors that need it.
	_ = ctx
	cmd := exec.Command(args[0], args[1:]...)
	cmd.Dir = req.Worktree
	cmd.Env = req.Env
	return cmd, nil
}

// templateVars lists the {{name}} placeholders in an argument.
func templateVars(arg string) []string {
	var out []string
	for {
		i := strings.Index(arg, "{{")
		if i < 0 {
			return out
		}
		j := strings.Index(arg[i:], "}}")
		if j < 0 {
			return out
		}
		out = append(out, arg[i+2:i+j])
		arg = arg[i+j+2:]
	}
}

func render(arg string, vars map[string]string) string {
	for k, v := range vars {
		arg = strings.ReplaceAll(arg, "{{"+k+"}}", v)
	}
	return arg
}

// Executors is the registry by name.
type Executors map[string]Executor

// ExecutorsFromConfig builds the registry from worker.toml.
func ExecutorsFromConfig(cfgs map[string]ExecutorConfig) (Executors, error) {
	out := Executors{}
	for name, cfg := range cfgs {
		e, err := NewTemplateExecutor(name, cfg)
		if err != nil {
			return nil, err
		}
		out[name] = e
	}
	return out, nil
}

// Names returns the registered names, sorted (for registration).
func (e Executors) Names() []string {
	names := make([]string, 0, len(e))
	for n := range e {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}

// PassthroughEnv filters the parent environment to the DESIGN §1.1 allow-list
// and appends extra entries; it is the only way a child process gets an
// environment from Forge.
func PassthroughEnv(parent []string, extra ...string) []string {
	allowed := func(key string) bool {
		switch key {
		case "PATH", "HOME", "USER", "LANG", "TERM", "SSH_AUTH_SOCK", "CLAUDE_CONFIG_DIR", "FORGE_HOME", "FORGE_HTTP":
			return true
		}
		for _, prefix := range []string{"LC_", "XDG_", "ANTHROPIC_", "FORGE_LOG_"} {
			if strings.HasPrefix(key, prefix) {
				return true
			}
		}
		return false
	}
	out := make([]string, 0, len(parent)+len(extra))
	for _, kv := range parent {
		key, _, ok := strings.Cut(kv, "=")
		if ok && allowed(key) {
			out = append(out, kv)
		}
	}
	return append(out, extra...)
}
