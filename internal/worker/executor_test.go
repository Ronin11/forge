package worker

import (
	"strings"
	"testing"

	"forge/internal/protocol"
)

func TestExecutorRender(t *testing.T) {
	e := ExecutorConfig{
		Command:          []string{"claude", "--model", "{{model}}", "--max-turns", "{{max_turns}}", "--cwd={{worktree}}"},
		Output:           "claude-stream-json",
		AllowedToolsFlag: "--allowedTools",
	}
	if err := e.Validate("c"); err != nil {
		t.Fatal(err)
	}
	argv, err := e.Render(protocol.Settings{Model: "haiku", MaxTurns: 5, AllowedTools: []string{"Read", "Bash"}}, "equitizr", "/wt")
	if err != nil {
		t.Fatal(err)
	}
	want := "claude --model haiku --max-turns 5 --cwd=/wt --allowedTools Read,Bash"
	if got := strings.Join(argv, " "); got != want {
		t.Errorf("got %q want %q", got, want)
	}
	e.AllowedToolsFlag = ""
	argv, _ = e.Render(protocol.Settings{Model: "haiku", AllowedTools: []string{"Read"}}, "r", "/wt")
	if len(argv) != 6 {
		t.Errorf("allowed tools passed without capability: %v", argv)
	}
	if _, err := e.Render(protocol.Settings{}, "r", "/wt"); err == nil {
		t.Error("empty model accepted")
	}
	bad := ExecutorConfig{Command: []string{"x", "{{nope}}"}, Output: "lines"}
	if err := bad.Validate("bad"); err == nil {
		t.Error("unknown template variable accepted")
	}
	if err := (ExecutorConfig{Command: []string{"x"}, Output: "zzz"}).Validate("bad"); err == nil {
		t.Error("unknown parser accepted")
	}
}
