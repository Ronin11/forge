package tools_test

import (
	"context"
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"forge/internal/tools"
)

// writeScriptTool lays out one <root>/<dir>/ script tool on disk.
func writeScriptTool(t *testing.T, root, dir, manifest string, files map[string]string) {
	t.Helper()
	d := filepath.Join(root, dir)
	if err := os.MkdirAll(d, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(d, "manifest.toml"), []byte(manifest), 0o644); err != nil {
		t.Fatal(err)
	}
	for name, body := range files {
		perm := os.FileMode(0o644)
		if strings.HasSuffix(name, ".sh") {
			perm = 0o755
		}
		if err := os.WriteFile(filepath.Join(d, name), []byte(body), perm); err != nil {
			t.Fatal(err)
		}
	}
}

const echoManifest = `name = "echoer"
description = "echoes its input"
input_schema = '{"type":"object","additionalProperties":true}'
command = ["./run.sh"]
test_command = ["./test.sh"]
`

func TestLoadScriptTools(t *testing.T) {
	root := t.TempDir()
	writeScriptTool(t, root, "echoer", echoManifest, map[string]string{
		"run.sh":  "#!/bin/sh\ncat\n",
		"test.sh": "#!/bin/sh\nexit 0\n",
	})
	// A tool whose test fails is skipped, never loaded.
	writeScriptTool(t, root, "broken", strings.ReplaceAll(echoManifest, "echoer", "broken"), map[string]string{
		"run.sh":  "#!/bin/sh\ncat\n",
		"test.sh": "#!/bin/sh\nexit 1\n",
	})
	// A manifest whose name disagrees with its directory is skipped.
	writeScriptTool(t, root, "mismatch", echoManifest, map[string]string{
		"run.sh":  "#!/bin/sh\ncat\n",
		"test.sh": "#!/bin/sh\nexit 0\n",
	})

	got := tools.LoadScriptTools(context.Background(), root, slog.New(slog.DiscardHandler))
	if len(got) != 1 || got[0].Name() != "echoer" {
		t.Fatalf("loaded = %v, want only echoer", names(got))
	}
	tool := got[0]
	if tool.Where() != tools.WhereDaemon {
		t.Errorf("Where = %q, want daemon", tool.Where())
	}
	if tool.Description() != "echoes its input" {
		t.Errorf("Description = %q", tool.Description())
	}
	var schema map[string]any
	if err := json.Unmarshal(tool.InputSchema(), &schema); err != nil {
		t.Errorf("InputSchema: %v", err)
	}
	out, err := tool.Call(context.Background(), tools.Request{Input: json.RawMessage(`{"a":1}`)})
	if err != nil {
		t.Fatal(err)
	}
	var m map[string]int
	if err := json.Unmarshal(out, &m); err != nil {
		t.Fatalf("decode %q: %v", out, err)
	}
	if m["a"] != 1 {
		t.Errorf("Call = %s, want the piped-through input", out)
	}
}

func TestLoadScriptToolsMissingDir(t *testing.T) {
	got := tools.LoadScriptTools(context.Background(), filepath.Join(t.TempDir(), "none"), slog.New(slog.DiscardHandler))
	if got != nil {
		t.Errorf("loaded from a missing dir = %v", names(got))
	}
}

func TestScriptToolRejectsNonObjectOutput(t *testing.T) {
	root := t.TempDir()
	writeScriptTool(t, root, "echoer", echoManifest, map[string]string{
		"run.sh":  "#!/bin/sh\necho not json\n",
		"test.sh": "#!/bin/sh\nexit 0\n",
	})
	got := tools.LoadScriptTools(context.Background(), root, slog.New(slog.DiscardHandler))
	if len(got) != 1 {
		t.Fatalf("loaded = %v", names(got))
	}
	if _, err := got[0].Call(context.Background(), tools.Request{}); err == nil {
		t.Error("Call with non-JSON stdout succeeded, want error")
	}
}

func names(ts []tools.Tool) []string {
	var out []string
	for _, t := range ts {
		out = append(out, t.Name())
	}
	return out
}
