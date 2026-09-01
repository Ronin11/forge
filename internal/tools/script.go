package tools

// Script tools are proposal-installed executables under <home>/tools/<name>/
// (DESIGN.md §12): an approved `tool` proposal writes the directory, and the
// daemon loads every directory whose manifest is valid and whose test passes.
// The daemon pipes the tool input to the command's stdin and takes stdout as
// the JSON result, so a script tool is any program in any language.

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/BurntSushi/toml"

	"forge/internal/core/model"
)

// ScriptManifest is <home>/tools/<name>/manifest.toml — the one description of
// a script tool, written by the proposal apply engine (controlplane/apply.go)
// and read back by LoadScriptTools. InputSchema holds the JSON Schema verbatim
// as a string because TOML has no JSON-object type.
type ScriptManifest struct {
	Name           string   `toml:"name"`
	Description    string   `toml:"description"`
	InputSchema    string   `toml:"input_schema"`
	Command        []string `toml:"command"`
	TimeoutSeconds int      `toml:"timeout_seconds"`
	TestCommand    []string `toml:"test_command"`
}

// scriptTimeout caps a script tool's test run and is the default per-call cap,
// so a hung script can never stall the daemon or its startup.
const scriptTimeout = 60 * time.Second

// LoadScriptTools scans dir (<home>/tools) for <name>/manifest.toml and
// returns one daemon tool per valid directory whose test passes. A bad or
// failing tool is logged and skipped — a broken script must never keep the
// daemon from starting. A missing dir simply means no script tools yet.
func LoadScriptTools(ctx context.Context, dir string, log *slog.Logger) []Tool {
	entries, err := os.ReadDir(dir)
	if err != nil {
		if !os.IsNotExist(err) {
			log.WarnContext(ctx, "read script tool dir", "dir", dir, "error", err)
		}
		return nil
	}
	var out []Tool
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		toolDir := filepath.Join(dir, e.Name())
		m, err := loadManifest(toolDir)
		if os.IsNotExist(err) {
			continue // a directory without a manifest is not a tool
		}
		if err != nil {
			log.WarnContext(ctx, "script tool skipped", "dir", toolDir, "error", err)
			continue
		}
		if len(m.TestCommand) > 0 {
			if err := ScriptTest(ctx, toolDir, m.TestCommand); err != nil {
				log.WarnContext(ctx, "script tool skipped: test failed", "tool", m.Name, "error", err)
				continue
			}
		}
		out = append(out, &scriptTool{dir: toolDir, manifest: m})
		log.InfoContext(ctx, "script tool loaded", "tool", m.Name, "dir", toolDir)
	}
	return out
}

// loadManifest reads and validates one manifest.toml. The name must equal the
// directory base so the registry, the filesystem, and the manifest agree.
func loadManifest(dir string) (ScriptManifest, error) {
	var m ScriptManifest
	path := filepath.Join(dir, "manifest.toml")
	if _, err := os.Stat(path); err != nil {
		return m, err
	}
	if _, err := toml.DecodeFile(path, &m); err != nil {
		return m, fmt.Errorf("decode %s: %w", path, err)
	}
	if err := model.ValidateName(m.Name); err != nil {
		return m, err
	}
	if m.Name != filepath.Base(dir) {
		return m, fmt.Errorf("manifest name %q != directory %q", m.Name, filepath.Base(dir))
	}
	if len(m.Command) == 0 {
		return m, fmt.Errorf("manifest %s: command is required", m.Name)
	}
	var schema map[string]json.RawMessage
	if err := json.Unmarshal([]byte(m.InputSchema), &schema); err != nil {
		return m, fmt.Errorf("manifest %s: input_schema is not a JSON object: %w", m.Name, err)
	}
	if m.TimeoutSeconds <= 0 {
		m.TimeoutSeconds = int(scriptTimeout / time.Second)
	}
	return m, nil
}

// ScriptTest runs a tool's test_command in its directory with a bare
// environment (PATH only) — the "its tests pass" gate both a tool proposal's
// apply and the loader enforce (DESIGN.md §12).
func ScriptTest(ctx context.Context, dir string, command []string) error {
	tctx, cancel := context.WithTimeout(ctx, scriptTimeout)
	defer cancel()
	cmd := exec.CommandContext(tctx, command[0], command[1:]...)
	cmd.Dir = dir
	cmd.Env = []string{"PATH=" + os.Getenv("PATH")}
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("test %v: %v: %s", command, err, outputTail(out))
	}
	return nil
}

// scriptTool adapts one manifest to the Tool interface.
type scriptTool struct {
	dir      string
	manifest ScriptManifest
}

func (t *scriptTool) Name() string        { return t.manifest.Name }
func (t *scriptTool) Description() string { return t.manifest.Description }

func (t *scriptTool) InputSchema() json.RawMessage {
	return json.RawMessage(t.manifest.InputSchema)
}

func (t *scriptTool) Where() string { return WhereDaemon }

// Call pipes the input object to the command's stdin, runs it in the tool
// directory with a PATH-only environment and the manifest timeout, and returns
// stdout, which must be one JSON object. stderr travels only in errors.
func (t *scriptTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	input := req.Input
	if len(input) == 0 {
		input = json.RawMessage("{}")
	}
	tctx, cancel := context.WithTimeout(ctx, time.Duration(t.manifest.TimeoutSeconds)*time.Second)
	defer cancel()
	cmd := exec.CommandContext(tctx, t.manifest.Command[0], t.manifest.Command[1:]...)
	cmd.Dir = t.dir
	cmd.Env = []string{"PATH=" + os.Getenv("PATH")}
	cmd.Stdin = bytes.NewReader(input)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("tool %s: %v: %s", t.manifest.Name, err, outputTail(stderr.Bytes()))
	}
	var obj map[string]json.RawMessage
	if err := json.Unmarshal(stdout.Bytes(), &obj); err != nil {
		return nil, fmt.Errorf("tool %s: stdout is not a JSON object: %v: %s", t.manifest.Name, err, outputTail(stderr.Bytes()))
	}
	return json.RawMessage(stdout.Bytes()), nil
}

// outputTail bounds child output quoted in errors and logs.
func outputTail(out []byte) string {
	const n = 1024
	s := strings.TrimSpace(string(out))
	if len(s) > n {
		s = "…" + s[len(s)-n:]
	}
	return s
}
