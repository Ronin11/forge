package worker

import (
	"encoding/json"
	"fmt"
	"strings"

	"forge/internal/protocol"
)

// Result is what a parser extracted from the executor's stdout.
type Result struct {
	Text     string
	Found    bool // a terminal result was seen
	IsError  bool
	NumTurns int
	Usage    *protocol.Usage
	CostUSD  *float64
}

// Parser consumes executor stdout one line at a time. Line returns a short
// summary for the event log ("" to log nothing). Result is read after exit.
type Parser interface {
	Line(line []byte) string
	Result() Result
}

var parsers = map[string]func() Parser{
	"claude-stream-json": func() Parser { return &claudeStreamParser{} },
	"lines":              func() Parser { return &linesParser{keep: 50} },
}

// NewParser returns a fresh parser by registry name.
func NewParser(name string) (Parser, error) {
	factory, ok := parsers[name]
	if !ok {
		return nil, fmt.Errorf("unknown output parser %q", name)
	}
	return factory(), nil
}

// linesParser is the generic fallback: the last N lines are the result.
type linesParser struct {
	keep  int
	lines []string
}

func (p *linesParser) Line(line []byte) string {
	text := string(line)
	p.lines = append(p.lines, text)
	if len(p.lines) > p.keep {
		p.lines = p.lines[1:]
	}
	return boundedText(text, protocol.MaxEventMessage)
}

func (p *linesParser) Result() Result {
	return Result{Text: strings.Join(p.lines, "\n"), Found: len(p.lines) > 0}
}

// claudeStreamParser reads `claude --print --output-format stream-json`.
type claudeStreamParser struct {
	result Result
}

type claudeEvent struct {
	Type    string `json:"type"`
	Subtype string `json:"subtype"`
	Message struct {
		Content []struct {
			Type  string          `json:"type"`
			Text  string          `json:"text"`
			Name  string          `json:"name"`
			Input json.RawMessage `json:"input"`
		} `json:"content"`
	} `json:"message"`
	// result fields
	Result       string   `json:"result"`
	IsError      bool     `json:"is_error"`
	NumTurns     int      `json:"num_turns"`
	TotalCostUSD *float64 `json:"total_cost_usd"`
	Usage        *struct {
		Input         int64 `json:"input_tokens"`
		Output        int64 `json:"output_tokens"`
		CacheRead     int64 `json:"cache_read_input_tokens"`
		CacheCreation int64 `json:"cache_creation_input_tokens"`
	} `json:"usage"`
}

func (p *claudeStreamParser) Line(line []byte) string {
	var ev claudeEvent
	if err := json.Unmarshal(line, &ev); err != nil {
		return boundedText(string(line), protocol.MaxEventMessage)
	}
	switch ev.Type {
	case "system":
		if ev.Subtype == "init" {
			return "session started"
		}
		return ""
	case "assistant":
		var parts []string
		for _, c := range ev.Message.Content {
			switch c.Type {
			case "text":
				parts = append(parts, oneLine(c.Text))
			case "tool_use":
				parts = append(parts, "tool "+c.Name+" "+oneLine(string(c.Input)))
			}
		}
		if len(parts) == 0 {
			return ""
		}
		return boundedText("assistant: "+strings.Join(parts, " | "), protocol.MaxEventMessage)
	case "user":
		for _, c := range ev.Message.Content {
			if c.Type == "tool_result" {
				return "tool result received"
			}
		}
		return ""
	case "result":
		p.result = Result{Text: ev.Result, Found: true, IsError: ev.IsError, NumTurns: ev.NumTurns, CostUSD: ev.TotalCostUSD}
		if ev.Usage != nil {
			p.result.Usage = &protocol.Usage{InputTokens: ev.Usage.Input, OutputTokens: ev.Usage.Output,
				CacheReadTokens: ev.Usage.CacheRead, CacheCreationTokens: ev.Usage.CacheCreation}
		}
		if ev.IsError {
			return boundedText("result (error): "+oneLine(ev.Result), protocol.MaxEventMessage)
		}
		return boundedText("result: "+oneLine(ev.Result), protocol.MaxEventMessage)
	}
	return ""
}

func (p *claudeStreamParser) Result() Result { return p.result }

func oneLine(s string) string {
	s = strings.Join(strings.Fields(s), " ")
	return boundedText(s, 300)
}
