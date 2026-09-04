package flow

// Subprocess script execution: the non-.js half of the library scripts. The
// contract mirrors the goja sandbox at the boundary — JSON ScriptInput on
// stdin, one JSON value on stdout, bounded time and output — but the process
// runs with the daemon's permissions (filesystem, network). That is the
// deliberate trade the operator makes by writing a .py/.sh script instead of
// a .js one; the tool: flag stays the gate for what agents may invoke.

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"forge/internal/core/store"
)

// DefaultExternalTimeoutMS / MaxExternalTimeoutMS bound subprocess scripts —
// looser than goja (a real interpreter may legitimately work for a while),
// still short enough that a workflow tick cannot hang the engine's driver.
const (
	DefaultExternalTimeoutMS = 60_000
	MaxExternalTimeoutMS     = 300_000
)

// ExternalTimeout clamps a subprocess script's timeout.
func ExternalTimeout(ms int) time.Duration {
	if ms <= 0 {
		ms = DefaultExternalTimeoutMS
	}
	if ms > MaxExternalTimeoutMS {
		ms = MaxExternalTimeoutMS
	}
	return time.Duration(ms) * time.Millisecond
}

// RunAny dispatches one library script: an empty interpreter is a goja
// source (in-process, sandboxed); otherwise the interpreter argv runs the
// file as a subprocess. timeoutMS 0 takes each runtime's default.
func RunAny(interpreter []string, path, source string, input ScriptInput, timeoutMS int) (json.RawMessage, error) {
	if len(interpreter) == 0 {
		return RunScript(source, input, ScriptTimeout(timeoutMS))
	}
	return RunExternal(interpreter, path, input, ExternalTimeout(timeoutMS))
}

// RunExternal executes interpreter... path with input as JSON on stdin and
// returns the single JSON value the script prints. Environment is PATH-only
// (configuration travels through the input, not ambient env); cwd is the
// script's directory.
func RunExternal(interpreter []string, path string, input ScriptInput, timeout time.Duration) (json.RawMessage, error) {
	payload, err := json.Marshal(input)
	if err != nil {
		return nil, fmt.Errorf("encode script input: %w", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	argv := append(append([]string(nil), interpreter...), path)
	cmd := exec.CommandContext(ctx, argv[0], argv[1:]...)
	// A killed interpreter can leave grandchildren holding the stdout pipe;
	// WaitDelay stops Wait from blocking on their inherited descriptors.
	cmd.WaitDelay = 2 * time.Second
	cmd.Dir = filepath.Dir(path)
	cmd.Env = []string{"PATH=" + os.Getenv("PATH")}
	cmd.Stdin = bytes.NewReader(payload)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		if ctx.Err() != nil {
			return nil, fmt.Errorf("script timeout after %s", timeout)
		}
		return nil, fmt.Errorf("script failed: %v: %s", err, tail(stderr.Bytes()))
	}
	out := bytes.TrimSpace(stdout.Bytes())
	if len(out) == 0 {
		return json.RawMessage("null"), nil
	}
	if len(out) > store.MaxNodeOutputBytes {
		return nil, fmt.Errorf("script output %d bytes exceeds %d", len(out), store.MaxNodeOutputBytes)
	}
	if !json.Valid(out) {
		return nil, fmt.Errorf("script stdout is not one JSON value: %s", tail(out))
	}
	return json.RawMessage(out), nil
}

// tail bounds child output quoted in errors.
func tail(b []byte) string {
	const n = 1024
	s := strings.TrimSpace(string(b))
	if len(s) > n {
		return "…" + s[len(s)-n:]
	}
	return s
}
