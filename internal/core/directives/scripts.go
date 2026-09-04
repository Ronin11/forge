package directives

// Library scripts: scripts/<name>.<ext>, any language. A .js file runs
// in-process in the goja sandbox (no filesystem, no network — the safe
// default the UI scaffolds); any other extension runs as a subprocess with
// the daemon's permissions, reading the JSON input on stdin and writing one
// JSON object to stdout. Metadata rides a comment header with identical keys
// either way:
//
//	// .js — a legal block comment, so the file runs exactly as authored:
//	/**forge
//	 * description: one line of what this computes
//	 * input: {"type":"object", ...}   (a JSON schema; may wrap lines)
//	 * timeout_ms: 10000
//	 * tool: true
//	 */
//	function main(input) { ... }
//
//	# .py/.sh/anything with #-comments — after an optional shebang:
//	#!/usr/bin/env python3
//	#forge
//	# description: one line of what this computes
//	# input: {"type":"object", ...}
//	# tool: true
//	import json, sys
//	print(json.dumps(handle(json.load(sys.stdin))))
//
// Keys parse with frontmatter strictness: unknown keys and orphan
// continuation lines are load errors, each key appears at most once, and
// `tool: true` requires a description and an input schema — a callable an
// agent cannot understand is a mistake, not a tool. A subprocess script's
// interpreter resolves at load (shebang first, else the extension map), so
// an unrunnable file is a refused load, not a run failure.

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"

	"github.com/dop251/goja"
)

// Timeout caps. Goja mirrors the workflow engine's cap; a subprocess may
// legitimately run longer (store owns the canonical constants; duplicated so
// directives does not import store).
const (
	maxScriptTimeoutMS   = 30_000
	maxExternalTimeoutMS = 300_000
)

// interpreters maps extensions to argv prefixes for shebang-less files.
var interpreters = map[string][]string{
	".py": {"python3"},
	".sh": {"bash"},
	".rb": {"ruby"},
	".pl": {"perl"},
}

var scriptKeyLine = regexp.MustCompile(`^([a-z_]+):\s*(.*)$`)

// parseScript builds a script Fragment. Body keeps the full raw source; a
// .js body is goja-compiled, a subprocess body gets its interpreter resolved
// — either failure is a load error, never a run failure.
func parseScript(raw []byte, path, name string) (*Fragment, error) {
	sum := sha256.Sum256(raw)
	f := &Fragment{Name: name, Path: path, Hash: hex.EncodeToString(sum[:]), Script: true, Body: string(raw)}
	external := filepath.Ext(path) != ".js"
	var err error
	if external {
		err = parseHashHeader(f, path)
	} else {
		err = parseSlashHeader(f, path)
	}
	if err != nil {
		return nil, err
	}
	if external {
		f.Interpreter, err = resolveInterpreter(f.Body, path)
		if err != nil {
			return nil, err
		}
	} else {
		if _, err := goja.Compile(name, f.Body, true); err != nil {
			return nil, fmt.Errorf("%s: does not compile: %v", path, err)
		}
		if !strings.Contains(f.Body, "function main") {
			return nil, fmt.Errorf("%s: must define function main(input)", path)
		}
	}
	if f.Tool {
		if f.Description == "" {
			return nil, fmt.Errorf("%s: tool: true requires a description", path)
		}
		if f.InputSchema == "" {
			return nil, fmt.Errorf("%s: tool: true requires an input schema", path)
		}
	}
	return f, nil
}

// resolveInterpreter picks the argv prefix for a subprocess script: the
// shebang when present, else the extension map.
func resolveInterpreter(body, path string) ([]string, error) {
	if strings.HasPrefix(body, "#!") {
		line := body
		if i := strings.IndexByte(body, '\n'); i >= 0 {
			line = body[:i]
		}
		argv := strings.Fields(strings.TrimPrefix(line, "#!"))
		if len(argv) == 0 {
			return nil, fmt.Errorf("%s: empty shebang", path)
		}
		return argv, nil
	}
	if argv, ok := interpreters[filepath.Ext(path)]; ok {
		return append([]string(nil), argv...), nil
	}
	return nil, fmt.Errorf("%s: no shebang and no known interpreter for %q — add a #! line", path, filepath.Ext(path))
}

// parseSlashHeader extracts the /**forge ... */ block from a .js file.
func parseSlashHeader(f *Fragment, path string) error {
	body := f.Body
	if !strings.HasPrefix(body, "/**forge\n") && body != "/**forge" {
		return nil // headerless scripts are fine (non-tool)
	}
	end := strings.Index(body, "*/")
	if end < 0 {
		return fmt.Errorf("%s: unterminated /**forge header", path)
	}
	lines := strings.Split(body[len("/**forge\n"):end], "\n")
	for i, line := range lines {
		line = strings.TrimSpace(line)
		line = strings.TrimPrefix(line, "* ")
		lines[i] = strings.TrimSpace(strings.TrimPrefix(line, "*"))
	}
	return applyHeader(f, path, lines, maxScriptTimeoutMS)
}

// parseHashHeader extracts the #forge block from a #-commented file: an
// optional shebang, then `#forge`, then `# key: value` lines until the first
// line that is not a # comment.
func parseHashHeader(f *Fragment, path string) error {
	lines := strings.Split(f.Body, "\n")
	i := 0
	if len(lines) > 0 && strings.HasPrefix(lines[0], "#!") {
		i++
	}
	if i >= len(lines) || strings.TrimSpace(lines[i]) != "#forge" {
		return nil // headerless scripts are fine (non-tool)
	}
	i++
	var meta []string
	for ; i < len(lines); i++ {
		line := strings.TrimSpace(lines[i])
		if !strings.HasPrefix(line, "#") {
			break
		}
		meta = append(meta, strings.TrimSpace(strings.TrimPrefix(line, "#")))
	}
	return applyHeader(f, path, meta, maxExternalTimeoutMS)
}

// applyHeader parses the shared key grammar: a known `key:` starts a value,
// any other non-empty line continues the current key (joined with a space),
// unknown keys and orphan continuations are errors, each key at most once.
func applyHeader(f *Fragment, path string, lines []string, timeoutCapMS int) error {
	seen := map[string]bool{}
	current := ""
	values := map[string]string{}
	for _, line := range lines {
		if line == "" {
			continue
		}
		if m := scriptKeyLine.FindStringSubmatch(line); m != nil {
			key := m[1]
			switch key {
			case "description", "input", "timeout_ms", "tool":
				if seen[key] {
					return fmt.Errorf("%s: header key %q appears twice", path, key)
				}
				seen[key] = true
				current = key
				values[key] = m[2]
				continue
			default:
				return fmt.Errorf("%s: unknown header key %q", path, key)
			}
		}
		if current == "" {
			return fmt.Errorf("%s: header line %q before any key", path, line)
		}
		values[current] += " " + line
	}
	f.Description = strings.TrimSpace(values["description"])
	if raw := strings.TrimSpace(values["input"]); raw != "" {
		var obj map[string]json.RawMessage
		if err := json.Unmarshal([]byte(raw), &obj); err != nil {
			return fmt.Errorf("%s: header input is not a JSON object: %v", path, err)
		}
		f.InputSchema = raw
	}
	if raw := strings.TrimSpace(values["timeout_ms"]); raw != "" {
		n, err := strconv.Atoi(raw)
		if err != nil || n < 1 {
			return fmt.Errorf("%s: header timeout_ms %q: want a positive integer", path, raw)
		}
		if n > timeoutCapMS {
			n = timeoutCapMS
		}
		f.TimeoutMS = n
	}
	if raw := strings.TrimSpace(values["tool"]); raw != "" {
		switch raw {
		case "true":
			f.Tool = true
		case "false":
		default:
			return fmt.Errorf("%s: header tool %q: want true or false", path, raw)
		}
	}
	return nil
}
