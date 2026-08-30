package worker

import (
	"bufio"
	"os"
	"strings"
	"testing"
)

func TestClaudeStreamParserFixture(t *testing.T) {
	f, err := os.Open("testdata/claude-stream.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	p, err := NewParser("claude-stream-json")
	if err != nil {
		t.Fatal(err)
	}
	var summaries []string
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 1<<20)
	for sc.Scan() {
		if s := p.Line([]byte(sc.Text())); s != "" {
			summaries = append(summaries, s)
		}
	}
	r := p.Result()
	if !r.Found || r.IsError {
		t.Fatalf("result not found or error: %+v", r)
	}
	if !strings.Contains(r.Text, "Claude") {
		t.Errorf("result text %q", r.Text)
	}
	if r.NumTurns != 1 {
		t.Errorf("num_turns %d", r.NumTurns)
	}
	if r.CostUSD == nil || *r.CostUSD <= 0 {
		t.Errorf("cost %v", r.CostUSD)
	}
	if r.Usage == nil || r.Usage.InputTokens != 10 || r.Usage.OutputTokens != 72 ||
		r.Usage.CacheReadTokens != 13615 || r.Usage.CacheCreationTokens != 8698 {
		t.Errorf("usage %+v", r.Usage)
	}
	if len(summaries) < 3 || summaries[0] != "session started" || !strings.HasPrefix(summaries[len(summaries)-1], "result: ") {
		t.Errorf("summaries %q", summaries)
	}
	for _, s := range summaries {
		if strings.HasPrefix(s, "{") {
			t.Errorf("raw JSON leaked into summary: %q", s)
		}
	}
}

func TestClaudeStreamParserErrorAndGarbage(t *testing.T) {
	p, _ := NewParser("claude-stream-json")
	if s := p.Line([]byte("not json")); s != "not json" {
		t.Errorf("garbage summary %q", s)
	}
	p.Line([]byte(`{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}`))
	p.Line([]byte(`{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":5,"result":"boom"}`))
	r := p.Result()
	if !r.Found || !r.IsError || r.Text != "boom" || r.NumTurns != 5 || r.Usage != nil || r.CostUSD != nil {
		t.Errorf("error result %+v", r)
	}
}

func TestLinesParser(t *testing.T) {
	p, _ := NewParser("lines")
	for i := 0; i < 60; i++ {
		p.Line([]byte(strings.Repeat("x", i)))
	}
	r := p.Result()
	lines := strings.Split(r.Text, "\n")
	if len(lines) != 50 || lines[49] != strings.Repeat("x", 59) || !r.Found {
		t.Errorf("lines result: %d lines", len(lines))
	}
	if _, err := NewParser("nope"); err == nil {
		t.Error("unknown parser accepted")
	}
}
