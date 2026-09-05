package main

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/BurntSushi/toml"

	"forge/internal/core/logging"
	"forge/internal/tui"
)

// fakeMeta is DESIGN.md §7.4's meta.toml, field for field. Unknown keys are
// rejected so a typo in a fixture fails loudly instead of silently replaying with
// defaults.
type fakeMeta struct {
	ExitCode       int               `toml:"exit_code"`
	DelayMS        int               `toml:"delay_ms"`
	LingerMS       int               `toml:"linger_ms"` // stay alive after the last line (linger-watcher tests)
	ResumeScript   string            `toml:"resume_script"`
	NeedsInputAt   int               `toml:"needs_input_at"`
	RateLimitEvent *fakeRateLimit    `toml:"rate_limit_event"`
	RateLimits     []fakeRateLimitAt `toml:"rate_limit_events"`
	Files          []fakeFile        `toml:"files"`
	LineDelays     []fakeLineDelay   `toml:"line_delays"`
}

// fakeRateLimit is the utilisation of the two windows a rate_limit_event reports.
type fakeRateLimit struct {
	FiveHour float64 `toml:"five_hour"`
	SevenDay float64 `toml:"seven_day"`
}

// fakeRateLimitAt is one positioned rate_limit_event, emitted right after the
// named script line — so budget and scheduler tests can script utilisation
// rising mid-run deterministically. Line references are validated against
// whichever script this invocation replays (script.jsonl or resume_script).
type fakeRateLimitAt struct {
	AtLine   int     `toml:"at_line"`
	FiveHour float64 `toml:"five_hour"`
	SevenDay float64 `toml:"seven_day"`
}

// fakeFile is a side effect the fixture performs in the cwd when a line is emitted,
// so a fixture can leave the worktree in the state its stream claims.
type fakeFile struct {
	AtLine    int    `toml:"at_line"`
	Path      string `toml:"path"`
	Content   string `toml:"content"`
	GitCommit string `toml:"git_commit"`
}

// fakeLineDelay overrides delay_ms for the pause that precedes one line.
type fakeLineDelay struct {
	Line    int `toml:"line"`
	DelayMS int `toml:"delay_ms"`
}

// validate holds the fixture invariants relative to the script that will be
// emitted: every line reference must name a line that exists. needs_input_at is
// ignored on a resume — a resume answers the question, so a fixture that asked
// again with the same meta could never complete.
func (m fakeMeta) validate(lines int) error {
	if m.ExitCode < 0 || m.ExitCode > 255 {
		return fmt.Errorf("exit_code %d is not in 0..255", m.ExitCode)
	}
	if m.DelayMS < 0 {
		return fmt.Errorf("delay_ms %d is negative", m.DelayMS)
	}
	if m.NeedsInputAt < 0 || m.NeedsInputAt > lines {
		return fmt.Errorf("needs_input_at %d is not in 0..%d", m.NeedsInputAt, lines)
	}
	if r := m.RateLimitEvent; r != nil && (r.FiveHour < 0 || r.SevenDay < 0) {
		return errors.New("rate_limit_event utilisations must not be negative")
	}
	for _, r := range m.RateLimits {
		if r.AtLine < 1 || r.AtLine > lines {
			return fmt.Errorf("rate_limit_events: at_line %d is not in 1..%d", r.AtLine, lines)
		}
		if r.FiveHour < 0 || r.SevenDay < 0 {
			return fmt.Errorf("rate_limit_events: utilisations for line %d must not be negative", r.AtLine)
		}
	}
	for _, f := range m.Files {
		if f.AtLine < 1 || f.AtLine > lines {
			return fmt.Errorf("files: at_line %d is not in 1..%d", f.AtLine, lines)
		}
		if f.Path == "" || !filepath.IsLocal(f.Path) {
			return fmt.Errorf("files: path %q must be relative and stay inside the cwd", f.Path)
		}
	}
	for _, d := range m.LineDelays {
		if d.Line < 1 || d.Line > lines {
			return fmt.Errorf("line_delays: line %d is not in 1..%d", d.Line, lines)
		}
		if d.DelayMS < 0 {
			return fmt.Errorf("line_delays: delay_ms %d for line %d is negative", d.DelayMS, d.Line)
		}
	}
	return nil
}

// fakeFixture is a loaded fixture directory: its meta and the one script this
// invocation replays (script.jsonl, or resume_script on --resume).
type fakeFixture struct {
	name    string
	meta    fakeMeta
	lines   []string
	resumed bool
}

// loadFixture reads and validates a fixture directory. Any problem is a usage
// error (exit 2) for the caller: the fixture is part of the test, not of the run.
func loadFixture(dir string, resumed bool) (*fakeFixture, error) {
	name := filepath.Base(dir)
	if st, err := os.Stat(dir); err != nil || !st.IsDir() {
		return nil, fmt.Errorf("fixture %s: %s is not a directory", name, dir)
	}
	fx := &fakeFixture{name: name, resumed: resumed}
	md, err := toml.DecodeFile(filepath.Join(dir, "meta.toml"), &fx.meta)
	if err != nil {
		return nil, fmt.Errorf("fixture %s: meta.toml: %w", name, err)
	}
	if undecoded := md.Undecoded(); len(undecoded) > 0 {
		return nil, fmt.Errorf("fixture %s: meta.toml: unknown key %s", name, undecoded[0])
	}
	script := "script.jsonl"
	if resumed && fx.meta.ResumeScript != "" {
		script = fx.meta.ResumeScript
	}
	raw, err := os.ReadFile(filepath.Join(dir, script))
	if err != nil {
		return nil, fmt.Errorf("fixture %s: %w", name, err)
	}
	// Only the final newline is framing; an interior blank line is kept because a
	// fixture may deliberately feed the parser garbage.
	fx.lines = strings.Split(strings.TrimSuffix(string(raw), "\n"), "\n")
	if resumed {
		// A resume answers the question; the pause point belongs to the first script.
		fx.meta.NeedsInputAt = 0
	}
	if err := fx.meta.validate(len(fx.lines)); err != nil {
		return nil, fmt.Errorf("fixture %s: meta.toml: %w", name, err)
	}
	return fx, nil
}

// scanFakeArgs keeps only the flags fake-claude understands (--fixture, --resume,
// the logging flags, -h) so a real FlagSet can parse them. Everything the executor
// template passes for the real CLI is dropped, together with its value when the
// next argument does not look like a flag; a positional prompt is dropped too.
// The flag package cannot do this itself: it stops at the first unknown flag.
func scanFakeArgs(args []string) []string {
	valued := map[string]bool{"fixture": true, "resume": true, "log-level": true, "log-format": true}
	boolean := map[string]bool{"v": true, "vv": true, "h": true, "help": true}
	var kept []string
	for i := 0; i < len(args); i++ {
		a := args[i]
		if !strings.HasPrefix(a, "-") {
			continue
		}
		name, _, hasValue := strings.Cut(strings.TrimLeft(a, "-"), "=")
		switch {
		case boolean[name]:
			kept = append(kept, a)
		case valued[name]:
			kept = append(kept, a)
			if !hasValue && i+1 < len(args) {
				kept = append(kept, args[i+1])
				i++
			}
		default:
			if !hasValue && i+1 < len(args) && !strings.HasPrefix(args[i+1], "-") {
				i++
			}
		}
	}
	return kept
}

// runFakeClaude is the test executor of DESIGN.md §7.4: it stands in for `claude`
// behind the same template executor so the worker's launch, parse, and sandbox
// paths are exercised by a real child process without spending budget.
func runFakeClaude(ctx context.Context, c *tui.Context, args []string) int {
	fs, lf := c.Flags("fake-claude")
	var fixture, resume string
	fs.StringVar(&fixture, "fixture", "", "fixture directory (default $FORGE_FAKE_FIXTURE)")
	fs.StringVar(&resume, "resume", "", "session id to resume: replays resume_script under this id")
	if code := c.Parse(fs, scanFakeArgs(args)); code >= 0 {
		return code
	}
	if fixture == "" {
		fixture = c.Getenv("FORGE_FAKE_FIXTURE")
	}
	if fixture == "" {
		fmt.Fprintln(c.Stderr, "forge fake-claude: --fixture or FORGE_FAKE_FIXTURE is required")
		return 2
	}
	_, log, err := c.Logger(lf, logging.Config{}, "cli.fake-claude")
	if err != nil {
		fmt.Fprintln(c.Stderr, "forge fake-claude:", err)
		return 2
	}
	fx, err := loadFixture(fixture, resume != "")
	if err != nil {
		fmt.Fprintln(c.Stderr, "forge fake-claude:", err)
		return 2
	}
	cwd, err := os.Getwd()
	if err != nil {
		fmt.Fprintln(c.Stderr, "forge fake-claude: resolve cwd:", err)
		return 1
	}
	session := resume
	if session == "" {
		if session, err = newUUID(); err != nil {
			fmt.Fprintln(c.Stderr, "forge fake-claude: new session id:", err)
			return 1
		}
	}
	// The real CLI reads its prompt from stdin to EOF before it answers; a worker
	// that forgot to close stdin must hang here exactly as it would with claude.
	// tui.Context carries no stdin, so this is the one place os.Stdin is read.
	prompt, err := io.ReadAll(os.Stdin)
	if err != nil {
		fmt.Fprintln(c.Stderr, "forge fake-claude: read prompt:", err)
		return 1
	}
	// One stderr line at start so the worker's stderr capture is exercised.
	fmt.Fprintf(c.Stderr, "fake-claude: fixture %s\n", fx.name)
	log.DebugContext(ctx, "replaying fixture", "fixture", fx.name, "lines", len(fx.lines),
		"resumed", fx.resumed, "prompt_bytes", len(prompt))
	r := &fakeReplay{
		out:     c.Stdout,
		getenv:  c.Getenv,
		cwd:     cwd,
		session: session,
		fx:      fx,
		now:     time.Now,
		usage:   map[string]fakeUsage{},
	}
	code, err := r.run(ctx)
	if err != nil {
		fmt.Fprintln(c.Stderr, "forge fake-claude:", err)
	}
	return code
}

// fakeReplay emits one fixture script. It keeps the per-message usage it has
// emitted so a synthesised needs-input result can carry the same cumulative totals
// the real CLI would, computed by the parser's own rule (last write per message id).
type fakeReplay struct {
	out     io.Writer
	getenv  func(string) string
	cwd     string
	session string
	fx      *fakeFixture
	now     func() time.Time
	usage   map[string]fakeUsage
	started time.Time
}

// run replays the script and returns the process exit code. A returned error is
// something the fixture could not do (a write, a commit, a closed stdout) and maps
// to exit 1; the fixture's own exit_code is only honoured when it ran to the end.
func (r *fakeReplay) run(ctx context.Context) (int, error) {
	r.started = r.now()
	meta := r.fx.meta
	delays := map[int]int{}
	for _, d := range meta.LineDelays {
		delays[d.Line] = d.DelayMS
	}
	files := map[int][]fakeFile{}
	for _, f := range meta.Files {
		files[f.AtLine] = append(files[f.AtLine], f)
	}
	ratesAt := map[int][]fakeRateLimitAt{}
	for _, rl := range meta.RateLimits {
		ratesAt[rl.AtLine] = append(ratesAt[rl.AtLine], rl)
	}
	resultAt := 0
	for i, l := range r.fx.lines {
		if lineType(l) == "result" {
			resultAt = i + 1
			break
		}
	}
	expand := strings.NewReplacer(
		"{{SESSION}}", r.session,
		"{{CWD}}", jsonString(r.cwd),
		"{{NOW}}", strconv.FormatInt(r.now().Unix(), 10),
	)
	rateEmitted := false
	emitRateLimit := func() error {
		if meta.RateLimitEvent == nil || rateEmitted {
			return nil
		}
		rateEmitted = true
		return r.emitJSON(r.rateLimitEvent(*meta.RateLimitEvent))
	}
	for i, raw := range r.fx.lines {
		n := i + 1
		delay := meta.DelayMS
		if n == 1 {
			delay = 0
		}
		if d, ok := delays[n]; ok {
			delay = d
		}
		if err := sleepCtx(ctx, time.Duration(delay)*time.Millisecond); err != nil {
			return 1, fmt.Errorf("before line %d: %w", n, err)
		}
		if n == resultAt {
			if err := emitRateLimit(); err != nil {
				return 1, err
			}
		}
		line := expand.Replace(raw)
		r.observe(line)
		if err := r.emit(line); err != nil {
			return 1, err
		}
		for _, f := range files[n] {
			if err := r.writeFile(ctx, f); err != nil {
				return 1, fmt.Errorf("line %d: %w", n, err)
			}
		}
		for _, rl := range ratesAt[n] {
			if err := r.emitJSON(r.rateLimitEvent(fakeRateLimit{FiveHour: rl.FiveHour, SevenDay: rl.SevenDay})); err != nil {
				return 1, fmt.Errorf("line %d: %w", n, err)
			}
		}
		if !r.fx.resumed && meta.NeedsInputAt == n {
			if err := emitRateLimit(); err != nil {
				return 1, err
			}
			if err := r.emitJSON(r.needsInputResult()); err != nil {
				return 1, err
			}
			return 0, nil
		}
	}
	// A script without a result line still gets its rate_limit_event, at the end.
	if err := emitRateLimit(); err != nil {
		return 1, err
	}
	// linger_ms holds the process open AFTER its last line — simulating an
	// executor that delivered its result and then wedged (telemetry retry
	// loops), which the worker's linger watcher must grace-kill.
	if meta.LingerMS > 0 {
		if err := sleepCtx(ctx, time.Duration(meta.LingerMS)*time.Millisecond); err != nil {
			return 1, fmt.Errorf("linger: %w", err)
		}
	}
	return meta.ExitCode, nil
}

// emit writes one line and flushes if the writer buffers, so a reader sees each
// line as soon as the fixture's timing says it should.
func (r *fakeReplay) emit(line string) error {
	if _, err := io.WriteString(r.out, line+"\n"); err != nil {
		return fmt.Errorf("write stdout: %w", err)
	}
	if f, ok := r.out.(interface{ Flush() error }); ok {
		if err := f.Flush(); err != nil {
			return fmt.Errorf("flush stdout: %w", err)
		}
	}
	return nil
}

func (r *fakeReplay) emitJSON(v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return fmt.Errorf("encode synthesised line: %w", err)
	}
	return r.emit(string(b))
}

// observe records the usage of an assistant message, keyed by message id, from a
// line about to be emitted. Non-JSON lines are passed through untouched.
func (r *fakeReplay) observe(line string) {
	var msg struct {
		Type    string `json:"type"`
		Message struct {
			ID    string     `json:"id"`
			Usage *fakeUsage `json:"usage"`
		} `json:"message"`
	}
	if err := json.Unmarshal([]byte(line), &msg); err != nil || msg.Type != "assistant" || msg.Message.ID == "" || msg.Message.Usage == nil {
		return
	}
	r.usage[msg.Message.ID] = *msg.Message.Usage
}

// writeFile performs a [[files]] side effect. The commit runs with an explicit
// identity when the environment has none, because a fresh test repository has no
// user.name and git would refuse the commit.
func (r *fakeReplay) writeFile(ctx context.Context, f fakeFile) error {
	path := filepath.Join(r.cwd, f.Path)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("create directory for %s: %w", f.Path, err)
	}
	if err := os.WriteFile(path, []byte(f.Content), 0o644); err != nil {
		return fmt.Errorf("write %s: %w", f.Path, err)
	}
	if f.GitCommit == "" {
		return nil
	}
	env := os.Environ()
	for _, kv := range [][2]string{
		{"GIT_AUTHOR_NAME", "forge-fake"}, {"GIT_AUTHOR_EMAIL", "forge-fake@localhost"},
		{"GIT_COMMITTER_NAME", "forge-fake"}, {"GIT_COMMITTER_EMAIL", "forge-fake@localhost"},
	} {
		if r.getenv(kv[0]) == "" {
			env = append(env, kv[0]+"="+kv[1])
		}
	}
	for _, argv := range [][]string{
		{"add", "-A"},
		{"-c", "commit.gpgsign=false", "commit", "-q", "-m", f.GitCommit},
	} {
		cmd := exec.CommandContext(ctx, "git", argv...)
		cmd.Dir = r.cwd
		cmd.Env = env
		if out, err := cmd.CombinedOutput(); err != nil {
			return fmt.Errorf("git %s in %s: %w: %s", argv[0], r.cwd, err, bytes.TrimSpace(out))
		}
	}
	return nil
}

// fakeUsage is the subset of a message's usage the result line totals.
type fakeUsage struct {
	InputTokens              int `json:"input_tokens"`
	CacheCreationInputTokens int `json:"cache_creation_input_tokens"`
	CacheReadInputTokens     int `json:"cache_read_input_tokens"`
	OutputTokens             int `json:"output_tokens"`
}

// totals sums the last-seen usage of every message emitted so far, which is what
// the real CLI reports on its result line.
func (r *fakeReplay) totals() (fakeUsage, int) {
	var t fakeUsage
	for _, u := range r.usage {
		t.InputTokens += u.InputTokens
		t.CacheCreationInputTokens += u.CacheCreationInputTokens
		t.CacheReadInputTokens += u.CacheReadInputTokens
		t.OutputTokens += u.OutputTokens
	}
	turns := len(r.usage)
	if turns == 0 {
		turns = 1
	}
	return t, turns
}

// fakeEnvelope is MODES.md's result envelope; only needs_input varies here.
type fakeEnvelope struct {
	SchemaVersion int             `json:"schema_version"`
	Summary       string          `json:"summary"`
	NeedsInput    *fakeNeedsInput `json:"needs_input"`
	Changes       json.RawMessage `json:"changes"`
	ChecksRun     json.RawMessage `json:"checks_run"`
	Claims        json.RawMessage `json:"claims"`
}

type fakeNeedsInput struct {
	Question   string   `json:"question"`
	Options    []string `json:"options"`
	Context    string   `json:"context"`
	Checkpoint *string  `json:"checkpoint"`
}

// fakeResult is the result line in the shape of the real CLI's success result,
// reduced to the fields the parser reads (§7.5) plus the ones a human expects.
type fakeResult struct {
	Type             string          `json:"type"`
	Subtype          string          `json:"subtype"`
	IsError          bool            `json:"is_error"`
	DurationMS       int64           `json:"duration_ms"`
	DurationAPIMS    int64           `json:"duration_api_ms"`
	NumTurns         int             `json:"num_turns"`
	Result           string          `json:"result"`
	StructuredOutput fakeEnvelope    `json:"structured_output"`
	SessionID        string          `json:"session_id"`
	TotalCostUSD     float64         `json:"total_cost_usd"`
	Usage            fakeUsage       `json:"usage"`
	StopReason       string          `json:"stop_reason"`
	PermissionDenied json.RawMessage `json:"permission_denials"`
	UUID             string          `json:"uuid"`
}

// needsInputResult is the result emitted when needs_input_at cuts the script: the
// fixed question the spec names, with the totals of what was emitted before it.
func (r *fakeReplay) needsInputResult() fakeResult {
	env := fakeEnvelope{
		SchemaVersion: 1,
		Summary:       "needs input",
		NeedsInput:    &fakeNeedsInput{Question: "Which one?", Options: []string{"a", "b"}},
		Changes:       json.RawMessage("[]"),
		ChecksRun:     json.RawMessage("[]"),
		Claims:        json.RawMessage("[]"),
	}
	text, err := json.Marshal(env)
	if err != nil {
		// The envelope has no dynamic content; a failure here is a programming error.
		panic(fmt.Sprintf("encode needs-input envelope: %v", err))
	}
	usage, turns := r.totals()
	elapsed := time.Since(r.started).Milliseconds()
	return fakeResult{
		Type:             "result",
		Subtype:          "success",
		DurationMS:       elapsed,
		DurationAPIMS:    elapsed,
		NumTurns:         turns,
		Result:           string(text),
		StructuredOutput: env,
		SessionID:        r.session,
		TotalCostUSD:     haikuCostUSD(usage),
		Usage:            usage,
		StopReason:       "end_turn",
		PermissionDenied: json.RawMessage("[]"),
		UUID:             fixedUUID("result"),
	}
}

// haikuCostUSD prices usage at the Haiku 4.5 list rates so synthesised results
// carry a plausible, deterministic cost.
func haikuCostUSD(u fakeUsage) float64 {
	const perMillion = 1e6
	return (float64(u.InputTokens)*1.00 + float64(u.OutputTokens)*5.00 +
		float64(u.CacheCreationInputTokens)*1.25 + float64(u.CacheReadInputTokens)*0.10) / perMillion
}

// fakeRateLimitLine mirrors the real CLI's rate_limit_event.
type fakeRateLimitLine struct {
	Type      string            `json:"type"`
	Info      fakeRateLimitInfo `json:"rate_limit_info"`
	UUID      string            `json:"uuid"`
	SessionID string            `json:"session_id"`
}

type fakeRateLimitInfo struct {
	Status          string `json:"status"`
	ResetsAt        int64  `json:"resetsAt"`
	RateLimitType   string `json:"rateLimitType"`
	OverageStatus   string `json:"overageStatus"`
	OverageResetsAt int64  `json:"overageResetsAt"`
	IsUsingOverage  bool   `json:"isUsingOverage"`
	UnifiedWindows  struct {
		FiveHour fakeWindow `json:"five_hour"`
		SevenDay fakeWindow `json:"seven_day"`
	} `json:"unifiedWindows"`
}

type fakeWindow struct {
	Utilization float64 `json:"utilization"`
	ResetsAt    int64   `json:"resetsAt"`
}

func (r *fakeReplay) rateLimitEvent(rl fakeRateLimit) fakeRateLimitLine {
	now := r.now().Unix()
	l := fakeRateLimitLine{Type: "rate_limit_event", UUID: fixedUUID("rate"), SessionID: r.session}
	l.Info = fakeRateLimitInfo{
		Status: "allowed", ResetsAt: now + 3600, RateLimitType: "five_hour",
		OverageStatus: "allowed", OverageResetsAt: now + 86400,
	}
	l.Info.UnifiedWindows.FiveHour = fakeWindow{Utilization: rl.FiveHour, ResetsAt: now + 3600}
	l.Info.UnifiedWindows.SevenDay = fakeWindow{Utilization: rl.SevenDay, ResetsAt: now + 86400}
	return l
}

// lineType is the "type" of a JSON line, or "" for anything the parser would treat
// as plain output.
func lineType(line string) string {
	var v struct {
		Type string `json:"type"`
	}
	if err := json.Unmarshal([]byte(line), &v); err != nil {
		return ""
	}
	return v.Type
}

// jsonString is s escaped for splicing inside a JSON string literal, so a cwd
// containing a quote or backslash cannot break a fixture line.
func jsonString(s string) string {
	b, err := json.Marshal(s)
	if err != nil {
		return s
	}
	return strings.Trim(string(b), `"`)
}

// newUUID returns a random UUID v4, the session id format claude uses. It is not a
// Forge ID (model.NewID), which is why it does not live in model.
func newUUID() (string, error) {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16]), nil
}

// fixedUUID is a recognisably fake, stable uuid for synthesised lines, so diffs of
// two replays differ only in what actually changed.
func fixedUUID(kind string) string {
	return fmt.Sprintf("00000000-0000-4000-8000-%012x", len(kind)*0x1001)
}

// sleepCtx waits d or until ctx is cancelled, whichever first, so a long fixture
// delay is interruptible without killing the process.
func sleepCtx(ctx context.Context, d time.Duration) error {
	if d <= 0 {
		return ctx.Err()
	}
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-t.C:
		return nil
	}
}
