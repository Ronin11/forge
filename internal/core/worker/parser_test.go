package worker

import (
	"bufio"
	"bytes"
	"encoding/json"
	"os"
	"strings"
	"testing"

	"forge/internal/core/protocol"
)

// feed runs every line of stream through p and returns all events in order,
// asserting each one would pass the store's validation.
func feed(t *testing.T, p OutputParser, stream []byte) []protocol.Event {
	t.Helper()
	var events []protocol.Event
	sc := bufio.NewScanner(bytes.NewReader(stream))
	sc.Buffer(nil, 4<<20)
	for sc.Scan() {
		for _, e := range p.Line(sc.Bytes()) {
			if err := e.Validate(); err != nil {
				t.Fatalf("parser produced invalid event: %v (%+v)", err, e)
			}
			events = append(events, e)
		}
	}
	if err := sc.Err(); err != nil {
		t.Fatalf("scan stream: %v", err)
	}
	return events
}

func attrsOf(t *testing.T, e protocol.Event) map[string]any {
	t.Helper()
	m := map[string]any{}
	if len(e.Attrs) == 0 {
		return m
	}
	if err := json.Unmarshal(e.Attrs, &m); err != nil {
		t.Fatalf("attrs %s: %v", e.Attrs, err)
	}
	return m
}

func newClaudeParser(t *testing.T, agentSpan string) OutputParser {
	t.Helper()
	factory, ok := DefaultParsers()["claude-stream-json"]
	if !ok {
		t.Fatal("claude-stream-json parser not registered")
	}
	return factory(agentSpan)
}

func TestClaudeStreamSample(t *testing.T) {
	stream, err := os.ReadFile("../../../.scratch/claude-stream.jsonl")
	if err != nil {
		t.Skipf("sample stream unavailable: %v", err)
	}
	p := newClaudeParser(t, "agent-1")
	events := feed(t, p, stream)
	res := p.Result()

	if res.SessionID != "2459202a-9572-458a-9519-32234b0c3296" {
		t.Errorf("SessionID = %q", res.SessionID)
	}
	if res.Model != "claude-haiku-4-5-20251001" {
		t.Errorf("Model = %q", res.Model)
	}
	if len(res.Tools) != 120 || res.Tools[0] != "Task" || res.Tools[1] != "Bash" {
		t.Errorf("Tools = %d entries starting %v", len(res.Tools), res.Tools[:min(3, len(res.Tools))])
	}
	wantUsage := protocol.Usage{InputTokens: 10, OutputTokens: 72, CacheReadTokens: 13615, CacheCreationTokens: 8698}
	if res.Usage != wantUsage {
		t.Errorf("Usage = %+v, want %+v", res.Usage, wantUsage)
	}
	if !res.HasResult || res.IsError || res.NumTurns != 1 {
		t.Errorf("HasResult=%v IsError=%v NumTurns=%d", res.HasResult, res.IsError, res.NumTurns)
	}
	if res.CostUSD == nil || *res.CostUSD != 0.0191275 {
		t.Errorf("CostUSD = %v", res.CostUSD)
	}
	if !strings.HasPrefix(res.Text, "Hey!") {
		t.Errorf("Text = %q", res.Text)
	}
	if res.Structured != nil {
		t.Errorf("Structured = %s, want nil for plain text", res.Structured)
	}
	if len(res.Samples) != 2 {
		t.Fatalf("Samples = %+v", res.Samples)
	}
	if res.Samples[0].Window != "five_hour" || res.Samples[0].Utilization != 0.09 || res.Samples[0].ResetsAt.Unix() != 1788121800 {
		t.Errorf("five_hour sample = %+v", res.Samples[0])
	}
	if res.Samples[1].Window != "seven_day" || res.Samples[1].Utilization != 0.02 || res.Samples[1].ResetsAt.Unix() != 1788292800 {
		t.Errorf("seven_day sample = %+v", res.Samples[1])
	}
	if res.UnknownLines != 0 || res.DroppedLines != 0 {
		t.Errorf("UnknownLines=%d DroppedLines=%d", res.UnknownLines, res.DroppedLines)
	}

	// init lifecycle+metric, one usage metric (the two assistant lines share an
	// id), rate_limit metric, result lifecycle.
	var kinds []string
	for _, e := range events {
		kinds = append(kinds, e.Kind+"/"+e.Name)
	}
	want := []string{"lifecycle/", "metric/init", "metric/usage", "metric/rate_limit", "lifecycle/"}
	if strings.Join(kinds, " ") != strings.Join(want, " ") {
		t.Errorf("events = %v, want %v", kinds, want)
	}
	rl := attrsOf(t, events[3])
	if rl["five_hour_utilization"] != 0.09 || rl["seven_day_utilization"] != 0.02 {
		t.Errorf("rate_limit attrs = %v", rl)
	}
	usage := attrsOf(t, events[2])
	if usage["message_id"] != "msg_011CeZEe9ZGHuknGjyXv7s5G" || usage["output_tokens"] != float64(8) {
		t.Errorf("usage attrs = %v", usage)
	}
}

// step is one input line and the events it must produce, in order.
type step struct {
	name  string
	line  string
	kinds []string // "kind/name" per event
}

func TestClaudeStreamSynthetic(t *testing.T) {
	bashInput := `{"command":"git status --porcelain","description":"Show status"}`
	longLine := strings.Repeat("x", MaxLine+1)
	steps := []step{
		{"init", `{"type":"system","subtype":"init","session_id":"sess-1","model":"claude-x","tools":["Bash","Read"]}`,
			[]string{"lifecycle/", "metric/init"}},
		{"thinking_tokens ignored", `{"type":"system","subtype":"thinking_tokens","estimated_tokens":8}`, nil},
		{"assistant text", `{"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}}}`,
			[]string{"metric/usage"}},
		{"assistant bash (dup id)", `{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":` + bashInput + `}],"usage":{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}}}`,
			[]string{"span_start/Bash"}},
		{"assistant mcp", `{"type":"assistant","message":{"id":"msg_2","content":[{"type":"tool_use","id":"toolu_2","name":"mcp__forge__forge_repo_status","input":{"repo":"forge"}}],"usage":{"input_tokens":5,"output_tokens":6,"cache_read_input_tokens":7,"cache_creation_input_tokens":8}}}`,
			[]string{"metric/usage", "span_start/mcp__forge__forge_repo_status"}},
		{"tool_result bash", `{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"M parser.go\n"}]}}`,
			[]string{"span_end/Bash"}},
		{"tool_result mcp error", `{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","is_error":true,"content":[{"type":"text","text":"boom"}]}]}}`,
			[]string{"span_end/mcp__forge__forge_repo_status"}},
		{"orphan tool_result", `{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_9","content":"?"}]}}`,
			[]string{"metric/orphan_tool_result"}},
		{"plain user turn", `{"type":"user","message":{"content":"answer: yes"}}`, nil},
		{"over-long", longLine, []string{"stderr/"}},
		{"non-json", `warning: something on stdout`, []string{"stdout/"}},
		{"truncated json", `{"type":"assistant","message":{"id":`, []string{"stdout/"}},
		{"unknown type", `{"type":"stream_event","event":{}}`, nil},
		{"rate limit", `{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"utilization":0.5,"resetsAt":100},"seven_day":{"utilization":0.25,"resetsAt":200}}}}`,
			[]string{"metric/rate_limit"}},
		{"result malformed usage", `{"type":"result","subtype":"success","is_error":false,"num_turns":3,"result":"first","usage":{"input_tokens":1,"output_tokens":-1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}`,
			[]string{"stderr/", "lifecycle/"}},
		{"result structured", `{"type":"result","subtype":"success","is_error":true,"num_turns":4,"duration_ms":10,"duration_api_ms":7,"total_cost_usd":1.5,"result":"done","structured_output":{"ok": true},"usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":40}}`,
			[]string{"lifecycle/"}},
	}

	p := newClaudeParser(t, "agent-2")
	byName := map[string][]protocol.Event{}
	for _, s := range steps {
		events := p.Line([]byte(s.line))
		var got []string
		for _, e := range events {
			if err := e.Validate(); err != nil {
				t.Errorf("%s: invalid event %+v: %v", s.name, e, err)
			}
			got = append(got, e.Kind+"/"+e.Name)
		}
		if strings.Join(got, " ") != strings.Join(s.kinds, " ") {
			t.Errorf("%s: events = %v, want %v", s.name, got, s.kinds)
		}
		byName[s.name] = events
	}

	bashStart := byName["assistant bash (dup id)"][0]
	bashEnd := byName["tool_result bash"][0]
	if bashStart.SpanID == "" || bashStart.SpanID != bashEnd.SpanID || len(bashStart.SpanID) != 8 {
		t.Errorf("bash span ids: start %q end %q", bashStart.SpanID, bashEnd.SpanID)
	}
	if bashStart.ParentID != "agent-2" || bashEnd.ParentID != "agent-2" {
		t.Errorf("bash parents: start %q end %q", bashStart.ParentID, bashEnd.ParentID)
	}
	if bashEnd.DurationUS != 0 {
		t.Errorf("span_end DurationUS = %d, want 0 (worker computes it)", bashEnd.DurationUS)
	}
	a := attrsOf(t, bashStart)
	if a["tool"] != "Bash" || a["input_summary"] != "git status --porcelain" || a["input_bytes"] != float64(len(bashInput)) || a["mcp"] != nil {
		t.Errorf("bash span_start attrs = %v", a)
	}
	a = attrsOf(t, bashEnd)
	if a["output_bytes"] != float64(len("M parser.go\n")) || a["is_error"] != false {
		t.Errorf("bash span_end attrs = %v", a)
	}

	mcpStart := byName["assistant mcp"][1]
	mcpEnd := byName["tool_result mcp error"][0]
	if mcpStart.SpanID != mcpEnd.SpanID || mcpStart.SpanID == bashStart.SpanID {
		t.Errorf("mcp span ids: start %q end %q bash %q", mcpStart.SpanID, mcpEnd.SpanID, bashStart.SpanID)
	}
	a = attrsOf(t, mcpStart)
	if a["mcp"] != true || a["input_summary"] != `{"repo":"forge"}` {
		t.Errorf("mcp span_start attrs = %v", a)
	}
	a = attrsOf(t, mcpEnd)
	if a["is_error"] != true || a["output_bytes"] != float64(len(`[{"type":"text","text":"boom"}]`)) {
		t.Errorf("mcp span_end attrs = %v", a)
	}

	a = attrsOf(t, byName["assistant mcp"][0])
	if a["message_id"] != "msg_2" || a["input_tokens"] != float64(5) {
		t.Errorf("second usage metric attrs = %v", a)
	}
	a = attrsOf(t, byName["orphan tool_result"][0])
	if a["tool_use_id"] != "toolu_9" || a["count"] != float64(1) {
		t.Errorf("orphan attrs = %v", a)
	}

	if msg := byName["over-long"][0].Message; !strings.Contains(msg, "dropped over-long line") || !strings.Contains(msg, "1048577 bytes") {
		t.Errorf("over-long message = %q", msg)
	}
	if msg := byName["non-json"][0].Message; msg != "warning: something on stdout" {
		t.Errorf("non-json message = %q", msg)
	}
	if msg := byName["result malformed usage"][0].Message; msg != "result usage malformed" {
		t.Errorf("malformed usage message = %q", msg)
	}
	a = attrsOf(t, byName["result structured"][0])
	if a["is_error"] != true || a["num_turns"] != float64(4) || a["subtype"] != "success" || a["duration_ms"] != float64(10) || a["duration_api_ms"] != float64(7) {
		t.Errorf("result lifecycle attrs = %v", a)
	}

	res := p.Result()
	if res.SessionID != "sess-1" || res.Model != "claude-x" || len(res.Tools) != 2 {
		t.Errorf("init fields = %q %q %v", res.SessionID, res.Model, res.Tools)
	}
	if !res.HasResult || !res.IsError || res.NumTurns != 4 || res.Text != "done" {
		t.Errorf("result fields: HasResult=%v IsError=%v NumTurns=%d Text=%q", res.HasResult, res.IsError, res.NumTurns, res.Text)
	}
	if res.CostUSD == nil || *res.CostUSD != 1.5 {
		t.Errorf("CostUSD = %v", res.CostUSD)
	}
	if string(res.Structured) != `{"ok": true}` {
		t.Errorf("Structured = %s", res.Structured)
	}
	want := protocol.Usage{InputTokens: 100, OutputTokens: 20, CacheReadTokens: 30, CacheCreationTokens: 40}
	if res.Usage != want {
		t.Errorf("Usage = %+v, want %+v (last result wins, never summed)", res.Usage, want)
	}
	if res.UnknownLines != 1 || res.DroppedLines != 1 {
		t.Errorf("UnknownLines=%d DroppedLines=%d, want 1 and 1", res.UnknownLines, res.DroppedLines)
	}
	if len(res.Samples) != 2 || res.Samples[0].Utilization != 0.5 || res.Samples[1].ResetsAt.Unix() != 200 {
		t.Errorf("Samples = %+v", res.Samples)
	}
}

func TestClaudeStreamResultText(t *testing.T) {
	cases := []struct {
		name           string
		line           string
		wantText       string
		wantStructured string
		wantCostNil    bool
	}{
		{"plain text", `{"type":"result","result":"hello","usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":1,"cache_creation_input_tokens":1}}`, "hello", "", true},
		{"json text becomes structured", `{"type":"result","result":"{\"a\":1}","total_cost_usd":0.5,"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":1,"cache_creation_input_tokens":1}}`, `{"a":1}`, `{"a":1}`, false},
		{"json array text stays text", `{"type":"result","result":"[1,2]","usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":1,"cache_creation_input_tokens":1}}`, "[1,2]", "", true},
		{"structured_output wins over text", `{"type":"result","result":"{\"a\":1}","structured_output":{"b":2},"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":1,"cache_creation_input_tokens":1}}`, `{"a":1}`, `{"b":2}`, true},
		{"missing usage", `{"type":"result","result":"x"}`, "x", "", true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			p := newClaudeParser(t, "agent-1")
			events := p.Line([]byte(tc.line))
			res := p.Result()
			if !res.HasResult || res.Text != tc.wantText || string(res.Structured) != tc.wantStructured {
				t.Errorf("Text=%q Structured=%s HasResult=%v", res.Text, res.Structured, res.HasResult)
			}
			if (res.CostUSD == nil) != tc.wantCostNil {
				t.Errorf("CostUSD = %v", res.CostUSD)
			}
			if tc.name == "missing usage" && (len(events) != 2 || events[0].Kind != protocol.KindStderr) {
				t.Errorf("missing usage events = %+v", events)
			}
		})
	}
}

// A steer delivered as the session ends can make the CLI open a bonus session
// and emit a second, text-only result frame. A structured result is only
// superseded by another structured result — a delivered supervise assessment
// was clobbered this way live (2026-09-05). Text-after-text keeps last-wins.
func TestClaudeStreamStructuredResultSurvivesTextFrame(t *testing.T) {
	p := newClaudeParser(t, "agent-1")
	p.Line([]byte(`{"type":"result","result":"done","structured_output":{"assessment":{"outcome":"revise"}},"num_turns":38,"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":1,"cache_creation_input_tokens":1}}`))
	events := p.Line([]byte(`{"type":"result","result":"I already called StructuredOutput","num_turns":2,"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":1,"cache_creation_input_tokens":1}}`))
	res := p.Result()
	if res.Text != "done" || !strings.Contains(string(res.Structured), `"revise"`) || res.NumTurns != 38 {
		t.Fatalf("text frame clobbered the structured result: Text=%q Structured=%s NumTurns=%d", res.Text, res.Structured, res.NumTurns)
	}
	if len(events) != 1 || events[0].Kind != protocol.KindLifecycle || !strings.Contains(string(events[0].Attrs), `"ignored":true`) {
		t.Fatalf("ignored frame events = %+v", events)
	}
	// A later structured frame is a deliberate resubmission: it wins.
	p.Line([]byte(`{"type":"result","result":"redone","structured_output":{"assessment":{"outcome":"done"}},"num_turns":40,"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":1,"cache_creation_input_tokens":1}}`))
	if res = p.Result(); !strings.Contains(string(res.Structured), `"done"`) || res.NumTurns != 40 {
		t.Fatalf("structured resubmission ignored: %s turns=%d", res.Structured, res.NumTurns)
	}
}

func TestClaudeStreamLongNonJSONLineIsCapped(t *testing.T) {
	p := newClaudeParser(t, "agent-1")
	line := strings.Repeat("é", protocol.MaxLineEventBytes) // 2 bytes each, well under MaxLine
	events := p.Line([]byte(line))
	if len(events) != 1 || events[0].Kind != protocol.KindStdout {
		t.Fatalf("events = %+v", events)
	}
	msg := events[0].Message
	if len(msg) > protocol.MaxLineEventBytes || len(msg) < protocol.MaxLineEventBytes-1 {
		t.Errorf("message length = %d", len(msg))
	}
	if !strings.HasSuffix(msg, "é") {
		t.Errorf("message cut inside a rune: ends %q", msg[len(msg)-2:])
	}
	if p.Result().DroppedLines != 0 {
		t.Errorf("a line under MaxLine must not count as dropped")
	}
}

func TestClaudeStreamInputSummary(t *testing.T) {
	cases := []struct {
		tool  string
		input string
		want  string
	}{
		{"Bash", `{"command":"ls -la"}`, "ls -la"},
		{"Read", `{"file_path":"/x/y.go","limit":5}`, "/x/y.go"},
		{"Edit", `{"file_path":"/x/y.go"}`, "/x/y.go"},
		{"Bash", `{"description":"no command"}`, `{"description":"no command"}`},
		{"Grep", `{"pattern":"` + strings.Repeat("a", 200) + `"}`, `{"pattern":"` + strings.Repeat("a", 108)},
	}
	for _, tc := range cases {
		if got := inputSummary(tc.tool, []byte(tc.input)); got != tc.want {
			t.Errorf("inputSummary(%s, %s) = %q, want %q", tc.tool, tc.input, got, tc.want)
		}
	}
}

func TestLinesParser(t *testing.T) {
	factory, ok := DefaultParsers()["lines"]
	if !ok {
		t.Fatal("lines parser not registered")
	}
	p := factory("agent-1")
	if res := p.Result(); res.HasResult || res.Text != "" {
		t.Errorf("fresh parser result = %+v", res)
	}
	lines := []string{"first", "", "second  ", "   ", strings.Repeat("y", MaxLine+2)}
	var kinds []string
	for _, l := range lines {
		for _, e := range p.Line([]byte(l)) {
			if err := e.Validate(); err != nil {
				t.Errorf("invalid event: %v", err)
			}
			kinds = append(kinds, e.Kind)
		}
	}
	want := []string{"stdout", "stdout", "stdout", "stdout", "stderr"}
	if strings.Join(kinds, " ") != strings.Join(want, " ") {
		t.Errorf("kinds = %v, want %v", kinds, want)
	}
	res := p.Result()
	if !res.HasResult || res.Text != "second" || res.DroppedLines != 1 {
		t.Errorf("result = HasResult=%v Text=%q Dropped=%d", res.HasResult, res.Text, res.DroppedLines)
	}
}

func TestDefaultParsersAreIndependent(t *testing.T) {
	// The registry is a value; two factories must not share state.
	a := newClaudeParser(t, "agent-1")
	b := newClaudeParser(t, "agent-1")
	a.Line([]byte(`{"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}}`))
	if ev := b.Line([]byte(`{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":""}]}}`)); len(ev) != 1 || ev[0].Name != "orphan_tool_result" {
		t.Errorf("parser b saw parser a's span: %+v", ev)
	}
}
