package flow

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// RunExternal: stdin carries the ScriptInput JSON, stdout must be one JSON
// value, timeouts and stderr surface as errors.
func TestRunExternal(t *testing.T) {
	dir := t.TempDir()
	write := func(name, body string) string {
		p := filepath.Join(dir, name)
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
		return p
	}
	echo := write("echo.sh", `cat | tr -d '\n'`)
	out, err := RunExternal([]string{"bash"}, echo, ScriptInput{Params: map[string]any{"n": 7}}, time.Second)
	if err != nil || !strings.Contains(string(out), `"n":7`) {
		t.Fatalf("echo = %s, %v", out, err)
	}

	// Non-JSON stdout is refused.
	junk := write("junk.sh", `echo not json`)
	if _, err := RunExternal([]string{"bash"}, junk, ScriptInput{}, time.Second); err == nil || !strings.Contains(err.Error(), "not one JSON") {
		t.Errorf("junk = %v", err)
	}

	// A failing script surfaces stderr.
	boom := write("boom.sh", `echo kaboom >&2; exit 3`)
	if _, err := RunExternal([]string{"bash"}, boom, ScriptInput{}, time.Second); err == nil || !strings.Contains(err.Error(), "kaboom") {
		t.Errorf("boom = %v", err)
	}

	// Timeout kills the process.
	slow := write("slow.sh", `sleep 5; echo '{}'`)
	start := time.Now()
	if _, err := RunExternal([]string{"bash"}, slow, ScriptInput{}, 300*time.Millisecond); err == nil || !strings.Contains(err.Error(), "timeout") {
		t.Errorf("slow = %v", err)
	}
	if time.Since(start) > 3*time.Second {
		t.Error("timeout did not kill the process promptly")
	}

	// Empty stdout is null.
	quiet := write("quiet.sh", `true`)
	if out, err := RunExternal([]string{"bash"}, quiet, ScriptInput{}, time.Second); err != nil || string(out) != "null" {
		t.Errorf("quiet = %s, %v", out, err)
	}
}

func TestExternalTimeoutClamp(t *testing.T) {
	if ExternalTimeout(0) != time.Minute || ExternalTimeout(999_999_999) != 5*time.Minute {
		t.Errorf("clamp = %v %v", ExternalTimeout(0), ExternalTimeout(999_999_999))
	}
}
