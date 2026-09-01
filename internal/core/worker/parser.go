package worker

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"strings"
	"time"
	"unicode/utf8"

	"forge/internal/core/protocol"
)

// MaxLine is the per-line byte cap of every parser (DESIGN.md §7.5). A longer
// line is dropped rather than parsed so one runaway tool result cannot make the
// worker hold megabytes of JSON; the drop is visible as a stderr event.
const MaxLine = 1 << 20

// OutputParser turns an executor's stdout lines into events and a final result.
// The worker assigns Seq, Time, ElapsedUS on every event it returns; parsers set
// Kind, Message, SpanID, ParentID, Name, DurationUS, Attrs only.
type OutputParser interface {
	Line(line []byte) []protocol.Event
	Result() ParseResult
}

// ParseResult is what the parser knows when stdout ends.
type ParseResult struct {
	SessionID    string
	Model        string
	Tools        []string
	Text         string          // result text (final "result" message)
	Structured   json.RawMessage // structured output when --json-schema was used
	NumTurns     int
	CostUSD      *float64 // nil when not reported
	IsError      bool
	Usage        protocol.Usage // authoritative totals from the result message
	HasResult    bool           // a result message was seen
	Samples      []RateLimitObservation
	UnknownLines int
	DroppedLines int // over-long lines dropped
}

// RateLimitObservation is one window of a rate_limit_event, kept so the worker
// can report samples (DESIGN.md §9.1) without re-reading the output file.
type RateLimitObservation struct {
	Window      string // "five_hour" | "seven_day"
	Utilization float64
	ResetsAt    time.Time
}

// ParserFactory builds a parser for one launch; agentSpanID is the span id tool
// spans are parented under ("agent-1" etc).
type ParserFactory func(agentSpanID string) OutputParser

// Parsers is the registry of output parsers by name. It is a value handed to
// worker.New, never a package-level variable (STYLE.md §1, §3).
type Parsers map[string]ParserFactory

// DefaultParsers returns the shipped parsers: "claude-stream-json" and "lines".
func DefaultParsers() Parsers {
	return Parsers{
		"claude-stream-json": func(agentSpanID string) OutputParser {
			return &claudeStreamParser{
				agentSpanID: agentSpanID,
				open:        map[string]openSpan{},
				seenUsage:   map[string]bool{},
			}
		},
		"lines": func(string) OutputParser { return &linesParser{} },
	}
}

// openSpan is a tool_use that has not yet seen its tool_result. The map key is
// the raw block id; SpanID is its hashed form so tool ids never leave the worker.
type openSpan struct {
	SpanID string
	Name   string
}

// claudeStreamParser decodes `claude --output-format stream-json` (DESIGN.md
// §7.5). It keeps no output bodies: attrs carry names, sizes, and short
// summaries only, so the store's attrs cap (protocol.MaxAttrsBytes) always holds.
type claudeStreamParser struct {
	agentSpanID string
	result      ParseResult
	// open maps tool_use block id → span, until the matching tool_result.
	open map[string]openSpan
	// seenUsage records message ids whose usage metric was already emitted; the
	// CLI repeats a message (with identical usage) once per content block.
	seenUsage map[string]bool
}

// envelope is the one field every stream-json line shares.
type envelope struct {
	Type string `json:"type"`
}

func (p *claudeStreamParser) Line(line []byte) []protocol.Event {
	if len(line) > MaxLine {
		p.result.DroppedLines++
		return []protocol.Event{lineEvent(protocol.KindStderr,
			fmt.Sprintf("dropped over-long line (%d bytes)", len(line)))}
	}
	var env envelope
	if err := json.Unmarshal(line, &env); err != nil {
		// Anything the CLI prints that is not stream-json (a warning, a partial
		// line at a crash) is still worth seeing on the timeline.
		return []protocol.Event{lineEvent(protocol.KindStdout, string(line))}
	}
	switch env.Type {
	case "system":
		return p.system(line)
	case "assistant":
		return p.assistant(line)
	case "user":
		return p.user(line)
	case "rate_limit_event":
		return p.rateLimit(line)
	case "result":
		return p.finalResult(line)
	default:
		p.result.UnknownLines++
		return nil
	}
}

func (p *claudeStreamParser) Result() ParseResult { return p.result }

// system handles the init line; other subtypes (thinking_tokens and whatever is
// added next) are progress noise, not unknown lines.
func (p *claudeStreamParser) system(line []byte) []protocol.Event {
	var msg struct {
		Subtype   string   `json:"subtype"`
		SessionID string   `json:"session_id"`
		Model     string   `json:"model"`
		Tools     []string `json:"tools"`
	}
	if err := json.Unmarshal(line, &msg); err != nil || msg.Subtype != "init" {
		return nil
	}
	p.result.SessionID = msg.SessionID
	p.result.Model = msg.Model
	p.result.Tools = msg.Tools
	attrs := encodeAttrs(map[string]any{
		"session_id": msg.SessionID,
		"model":      msg.Model,
		"tools":      len(msg.Tools),
	})
	return []protocol.Event{
		{Kind: protocol.KindLifecycle, Message: "session initialised", Attrs: attrs},
		{Kind: protocol.KindMetric, Name: "init", Message: "init", Attrs: attrs},
	}
}

// contentBlock is the union of the block fields the parser reads; unused ones
// stay zero for the other block types.
type contentBlock struct {
	Type      string          `json:"type"`
	ID        string          `json:"id"`          // tool_use
	Name      string          `json:"name"`        // tool_use
	Input     json.RawMessage `json:"input"`       // tool_use
	ToolUseID string          `json:"tool_use_id"` // tool_result
	Content   json.RawMessage `json:"content"`     // tool_result: string or array
	IsError   bool            `json:"is_error"`    // tool_result
}

func (p *claudeStreamParser) assistant(line []byte) []protocol.Event {
	var msg struct {
		Message struct {
			ID      string         `json:"id"`
			Content []contentBlock `json:"content"`
			Usage   *struct {
				InputTokens         int64 `json:"input_tokens"`
				OutputTokens        int64 `json:"output_tokens"`
				CacheReadTokens     int64 `json:"cache_read_input_tokens"`
				CacheCreationTokens int64 `json:"cache_creation_input_tokens"`
			} `json:"usage"`
		} `json:"message"`
	}
	if err := json.Unmarshal(line, &msg); err != nil {
		p.result.UnknownLines++
		return nil
	}
	var events []protocol.Event
	// The CLI emits the same message id once per content block, each carrying
	// the full message usage; only the first is a new fact. Totals are never
	// derived from these — the result line is authoritative.
	if u := msg.Message.Usage; u != nil && msg.Message.ID != "" && !p.seenUsage[msg.Message.ID] {
		p.seenUsage[msg.Message.ID] = true
		events = append(events, protocol.Event{
			Kind:    protocol.KindMetric,
			Name:    "usage",
			Message: "usage " + msg.Message.ID,
			Attrs: encodeAttrs(map[string]any{
				"message_id":                  msg.Message.ID,
				"input_tokens":                u.InputTokens,
				"output_tokens":               u.OutputTokens,
				"cache_read_input_tokens":     u.CacheReadTokens,
				"cache_creation_input_tokens": u.CacheCreationTokens,
			}),
		})
	}
	for _, block := range msg.Message.Content {
		if block.Type != "tool_use" || block.ID == "" {
			continue
		}
		span := openSpan{SpanID: spanIDFor(block.ID), Name: block.Name}
		p.open[block.ID] = span
		input := compactJSON(block.Input)
		attrs := map[string]any{
			"tool":          block.Name,
			"input_bytes":   len(input),
			"input_summary": inputSummary(block.Name, input),
		}
		if strings.HasPrefix(block.Name, "mcp__") {
			attrs["mcp"] = true
		}
		events = append(events, protocol.Event{
			Kind:     protocol.KindSpanStart,
			Message:  "tool_use " + block.Name,
			SpanID:   span.SpanID,
			ParentID: p.agentSpanID,
			Name:     span.Name,
			Attrs:    encodeAttrs(attrs),
		})
	}
	return events
}

// user closes tool spans. DurationUS is deliberately left 0: the parser has no
// clock, and the worker computes the duration from the monotonic ElapsedUS it
// stamped on the matching span_start and on this span_end (STYLE.md §3, Time).
func (p *claudeStreamParser) user(line []byte) []protocol.Event {
	var msg struct {
		Message struct {
			Content json.RawMessage `json:"content"`
		} `json:"message"`
	}
	if err := json.Unmarshal(line, &msg); err != nil {
		p.result.UnknownLines++
		return nil
	}
	// A plain user turn has a string content; only arrays carry tool results.
	var blocks []contentBlock
	if err := json.Unmarshal(msg.Message.Content, &blocks); err != nil {
		return nil
	}
	var events []protocol.Event
	orphans := 0
	for _, block := range blocks {
		if block.Type != "tool_result" {
			continue
		}
		span, ok := p.open[block.ToolUseID]
		if !ok {
			orphans++
			events = append(events, protocol.Event{
				Kind:    protocol.KindMetric,
				Name:    "orphan_tool_result",
				Message: "tool_result without an open span",
				Attrs: encodeAttrs(map[string]any{
					"tool_use_id": block.ToolUseID,
					"count":       orphans,
				}),
			})
			continue
		}
		delete(p.open, block.ToolUseID)
		events = append(events, protocol.Event{
			Kind:     protocol.KindSpanEnd,
			Message:  "tool_result " + span.Name,
			SpanID:   span.SpanID,
			ParentID: p.agentSpanID,
			Name:     span.Name,
			Attrs: encodeAttrs(map[string]any{
				"output_bytes": outputBytes(block.Content),
				"is_error":     block.IsError,
			}),
		})
	}
	return events
}

// rateLimitWindow is one of the CLI's unifiedWindows; resetsAt is unix seconds.
type rateLimitWindow struct {
	Utilization float64 `json:"utilization"`
	ResetsAt    int64   `json:"resetsAt"`
}

func (p *claudeStreamParser) rateLimit(line []byte) []protocol.Event {
	var msg struct {
		Info struct {
			Windows struct {
				FiveHour *rateLimitWindow `json:"five_hour"`
				SevenDay *rateLimitWindow `json:"seven_day"`
			} `json:"unifiedWindows"`
		} `json:"rate_limit_info"`
	}
	if err := json.Unmarshal(line, &msg); err != nil {
		p.result.UnknownLines++
		return nil
	}
	attrs := map[string]any{}
	for _, w := range []struct {
		name   string
		window *rateLimitWindow
	}{
		{"five_hour", msg.Info.Windows.FiveHour},
		{"seven_day", msg.Info.Windows.SevenDay},
	} {
		if w.window == nil {
			continue
		}
		attrs[w.name+"_utilization"] = w.window.Utilization
		attrs[w.name+"_resets_at"] = w.window.ResetsAt
		p.result.Samples = append(p.result.Samples, RateLimitObservation{
			Window:      w.name,
			Utilization: w.window.Utilization,
			ResetsAt:    time.Unix(w.window.ResetsAt, 0).UTC(),
		})
	}
	return []protocol.Event{{
		Kind:    protocol.KindMetric,
		Name:    "rate_limit",
		Message: "rate_limit",
		Attrs:   encodeAttrs(attrs),
	}}
}

// finalResult records the result line. A second result line replaces the first
// wholesale so a resumed session never mixes two runs' numbers.
func (p *claudeStreamParser) finalResult(line []byte) []protocol.Event {
	var msg struct {
		Subtype       string          `json:"subtype"`
		IsError       bool            `json:"is_error"`
		NumTurns      int             `json:"num_turns"`
		CostUSD       *float64        `json:"total_cost_usd"`
		Result        json.RawMessage `json:"result"`
		Structured    json.RawMessage `json:"structured_output"`
		Usage         json.RawMessage `json:"usage"`
		DurationMS    int64           `json:"duration_ms"`
		DurationAPIMS int64           `json:"duration_api_ms"`
	}
	if err := json.Unmarshal(line, &msg); err != nil {
		p.result.UnknownLines++
		return nil
	}
	p.result.HasResult = true
	p.result.IsError = msg.IsError
	p.result.NumTurns = msg.NumTurns
	p.result.CostUSD = msg.CostUSD
	p.result.Text = ""
	p.result.Structured = nil
	p.result.Usage = protocol.Usage{}

	// result is documented as a string; anything else is left as text-less.
	var text string
	if err := json.Unmarshal(msg.Result, &text); err == nil {
		p.result.Text = text
	}
	switch {
	case len(msg.Structured) > 0 && !bytes.Equal(msg.Structured, []byte("null")):
		p.result.Structured = append(json.RawMessage(nil), msg.Structured...)
	case isJSONObject([]byte(p.result.Text)):
		p.result.Structured = json.RawMessage(p.result.Text)
	}

	var events []protocol.Event
	usage, ok := parseUsage(msg.Usage)
	if ok {
		p.result.Usage = usage
	} else {
		events = append(events, lineEvent(protocol.KindStderr, "result usage malformed"))
	}
	events = append(events, protocol.Event{
		Kind:    protocol.KindLifecycle,
		Message: "result",
		Attrs: encodeAttrs(map[string]any{
			"is_error":        msg.IsError,
			"num_turns":       msg.NumTurns,
			"subtype":         msg.Subtype,
			"duration_ms":     msg.DurationMS,
			"duration_api_ms": msg.DurationAPIMS,
		}),
	})
	return events
}

// parseUsage accepts only the shape the attempt's totals depend on: all four
// counters present, integral, and non-negative. A partial usage is worse than
// none because it would be stored as authoritative.
func parseUsage(raw json.RawMessage) (protocol.Usage, bool) {
	if len(raw) == 0 {
		return protocol.Usage{}, false
	}
	var u struct {
		Input         *int64 `json:"input_tokens"`
		Output        *int64 `json:"output_tokens"`
		CacheRead     *int64 `json:"cache_read_input_tokens"`
		CacheCreation *int64 `json:"cache_creation_input_tokens"`
	}
	if err := json.Unmarshal(raw, &u); err != nil {
		return protocol.Usage{}, false
	}
	for _, v := range []*int64{u.Input, u.Output, u.CacheRead, u.CacheCreation} {
		if v == nil || *v < 0 {
			return protocol.Usage{}, false
		}
	}
	return protocol.Usage{
		InputTokens:         *u.Input,
		OutputTokens:        *u.Output,
		CacheReadTokens:     *u.CacheRead,
		CacheCreationTokens: *u.CacheCreation,
	}, true
}

// linesParser is the fallback for executors that print plain text: every line
// is a stdout event and the last non-empty line is the result.
type linesParser struct {
	result ParseResult
}

func (p *linesParser) Line(line []byte) []protocol.Event {
	if len(line) > MaxLine {
		p.result.DroppedLines++
		return []protocol.Event{lineEvent(protocol.KindStderr,
			fmt.Sprintf("dropped over-long line (%d bytes)", len(line)))}
	}
	p.result.HasResult = true
	if trimmed := strings.TrimSpace(string(line)); trimmed != "" {
		p.result.Text = trimmed
	}
	return []protocol.Event{lineEvent(protocol.KindStdout, string(line))}
}

func (p *linesParser) Result() ParseResult { return p.result }

// spanIDFor hashes a tool_use block id to the 8-hex-char span id of DESIGN.md
// §7.5, so span ids are short and the raw id never appears in the timeline.
func spanIDFor(blockID string) string {
	sum := sha256.Sum256([]byte(blockID))
	return hex.EncodeToString(sum[:])[:8]
}

// lineEvent builds a stdout/stderr event whose message respects the per-event
// cap; the store rejects longer messages and the timeline would not show them.
func lineEvent(kind, message string) protocol.Event {
	return protocol.Event{Kind: kind, Message: truncateBytes(message, protocol.MaxLineEventBytes)}
}

// encodeAttrs marshals a small, flat map. Marshalling a map of scalars cannot
// fail, but errcheck is right to insist: a failure is recorded rather than lost.
func encodeAttrs(m map[string]any) json.RawMessage {
	b, err := json.Marshal(m)
	if err != nil {
		return json.RawMessage(fmt.Sprintf(`{"attrs_error":%q}`, err.Error()))
	}
	return b
}

// compactJSON returns raw with insignificant whitespace removed, so input_bytes
// measures content rather than the CLI's formatting. Invalid JSON is returned as is.
func compactJSON(raw json.RawMessage) []byte {
	if len(raw) == 0 {
		return nil
	}
	var buf bytes.Buffer
	if err := json.Compact(&buf, raw); err != nil {
		return raw
	}
	return buf.Bytes()
}

// inputSummary is the one thing a reader wants to see for a tool call: the
// command for Bash, the path for file tools, otherwise the (short) input JSON.
func inputSummary(tool string, input []byte) string {
	var fields struct {
		Command  string `json:"command"`
		FilePath string `json:"file_path"`
	}
	summary := string(input)
	if err := json.Unmarshal(input, &fields); err == nil {
		switch {
		case tool == "Bash" && fields.Command != "":
			summary = fields.Command
		case (tool == "Read" || tool == "Edit") && fields.FilePath != "":
			summary = fields.FilePath
		}
	}
	return truncateRunes(summary, 120)
}

// outputBytes sizes a tool_result content: the decoded string when it is one,
// otherwise the compact JSON of the content array.
func outputBytes(content json.RawMessage) int {
	var s string
	if err := json.Unmarshal(content, &s); err == nil {
		return len(s)
	}
	return len(compactJSON(content))
}

// isJSONObject reports whether text is a complete JSON object, the only shape
// worth storing as a structured result.
func isJSONObject(text []byte) bool {
	trimmed := bytes.TrimSpace(text)
	return len(trimmed) > 0 && trimmed[0] == '{' && json.Valid(trimmed)
}

// truncateBytes cuts s to at most n bytes without splitting a UTF-8 sequence.
func truncateBytes(s string, n int) string {
	if len(s) <= n {
		return s
	}
	cut := n
	for cut > 0 && !utf8.RuneStart(s[cut]) {
		cut--
	}
	return s[:cut]
}

// truncateRunes cuts s to at most n runes; used for human-facing summaries
// where a byte count would cut a multi-byte character in half.
func truncateRunes(s string, n int) string {
	if utf8.RuneCountInString(s) <= n {
		return s
	}
	return string([]rune(s)[:n])
}
