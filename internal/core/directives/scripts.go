package directives

// Library scripts: scripts/<name>.js, goja JavaScript with an optional
// metadata header. The header is a legal JS block comment, so the file runs
// exactly as authored:
//
//	/**forge
//	 * description: one line of what this computes
//	 * input: {"type":"object", ...}   (a JSON schema; may wrap lines)
//	 * timeout_ms: 10000
//	 * tool: true
//	 */
//	function main(input) { ... }
//
// Keys parse with frontmatter strictness: unknown keys and orphan
// continuation lines are load errors, each key appears at most once, and
// `tool: true` requires a description and an input schema — a callable an
// agent cannot understand is a mistake, not a tool.

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"regexp"
	"strconv"
	"strings"

	"github.com/dop251/goja"
)

// MaxScriptTimeoutMS mirrors the workflow engine's cap (store owns the
// canonical constant; duplicated here so directives does not import store).
const maxScriptTimeoutMS = 30_000

var scriptKeyLine = regexp.MustCompile(`^([a-z_]+):\s*(.*)$`)

// parseScript builds a script Fragment: header metadata parsed, body kept as
// the full raw source (the header is a legal comment), and the whole thing
// compile-checked so a syntax error is a load error, never a run failure.
func parseScript(raw []byte, path, name string) (*Fragment, error) {
	sum := sha256.Sum256(raw)
	f := &Fragment{Name: name, Path: path, Hash: hex.EncodeToString(sum[:]), Script: true, Body: string(raw)}
	if err := parseScriptHeader(f, path); err != nil {
		return nil, err
	}
	if _, err := goja.Compile(name, f.Body, true); err != nil {
		return nil, fmt.Errorf("%s: does not compile: %v", path, err)
	}
	if !strings.Contains(f.Body, "function main") {
		return nil, fmt.Errorf("%s: must define function main(input)", path)
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

// parseScriptHeader extracts the /**forge ... */ block when present.
func parseScriptHeader(f *Fragment, path string) error {
	body := f.Body
	if !strings.HasPrefix(body, "/**forge\n") && body != "/**forge" {
		return nil // headerless scripts are fine (non-tool)
	}
	end := strings.Index(body, "*/")
	if end < 0 {
		return fmt.Errorf("%s: unterminated /**forge header", path)
	}
	lines := strings.Split(body[len("/**forge\n"):end], "\n")
	seen := map[string]bool{}
	current := ""
	values := map[string]string{}
	for _, line := range lines {
		line = strings.TrimSpace(line)
		line = strings.TrimPrefix(line, "* ")
		line = strings.TrimSpace(strings.TrimPrefix(line, "*"))
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
		if n > maxScriptTimeoutMS {
			n = maxScriptTimeoutMS
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
