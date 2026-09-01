package logging

import (
	"bytes"
	"context"
	"encoding/json"
	"flag"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

func TestParseLevels(t *testing.T) {
	cases := []struct {
		spec    string
		want    string
		wantErr bool
	}{
		{"info", "info", false},
		{"TRACE", "trace", false},
		{"debug,store=trace", "debug,store=trace", false},
		{"store=trace,worker=debug", "info,store=trace,worker=debug", false},
		{"", "info", false},
		{"loud", "", true},
		{"store=", "", true},
		{"=trace", "", true},
	}
	for _, c := range cases {
		got, err := ParseLevels(c.spec, slog.LevelInfo)
		if (err != nil) != c.wantErr {
			t.Errorf("ParseLevels(%q) err = %v, wantErr %v", c.spec, err, c.wantErr)
			continue
		}
		if err == nil && got.String() != c.want {
			t.Errorf("ParseLevels(%q) = %q, want %q", c.spec, got.String(), c.want)
		}
	}
}

func TestLevelsForUsesLongestPrefix(t *testing.T) {
	l := Levels{Default: slog.LevelInfo, Components: map[string]slog.Level{"worker": slog.LevelDebug, "worker.git": LevelTrace}}
	for component, want := range map[string]slog.Level{
		"store": slog.LevelInfo, "worker": slog.LevelDebug, "worker.git": LevelTrace,
		"worker.git.fetch": LevelTrace, "worker.manifest": slog.LevelDebug, "workers": slog.LevelInfo,
	} {
		if got := l.For(component); got != want {
			t.Errorf("For(%q) = %v, want %v", component, got, want)
		}
	}
}

func TestResolvePrecedence(t *testing.T) {
	env := func(vars map[string]string) func(string) string {
		return func(k string) string { return vars[k] }
	}
	cases := []struct {
		name   string
		flags  Flags
		env    map[string]string
		cfg    Config
		want   string
		format string
	}{
		{"default", Flags{}, nil, Config{}, "info", "text"},
		{"config", Flags{}, nil, Config{Level: "warn", Format: "json"}, "warn", "json"},
		{"env beats config", Flags{}, map[string]string{EnvLevel: "debug", EnvFormat: "text"}, Config{Level: "warn", Format: "json"}, "debug", "text"},
		{"flag beats env", Flags{Level: "error", Format: "json"}, map[string]string{EnvLevel: "debug"}, Config{Level: "warn"}, "error", "json"},
		{"-v beats level, keeps components", Flags{Level: "warn,store=trace", Verbose: true}, nil, Config{}, "debug,store=trace", "text"},
		{"-vv beats -v", Flags{Verbose: true, Trace: true}, nil, Config{}, "trace", "text"},
		{"-v never lowers verbosity", Flags{Level: "trace", Verbose: true}, nil, Config{}, "trace", "text"},
		{"winning spec replaces, no merge", Flags{Level: "store=trace"}, map[string]string{EnvLevel: "debug"}, Config{}, "info,store=trace", "text"},
		{"env is trimmed", Flags{}, map[string]string{EnvFormat: " json "}, Config{}, "info", "json"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got, err := Resolve(&c.flags, env(c.env), c.cfg, "/home/x/.forge")
			if err != nil {
				t.Fatal(err)
			}
			if got.Levels.String() != c.want || got.Format != c.format {
				t.Errorf("got %s/%s, want %s/%s", got.Levels.String(), got.Format, c.want, c.format)
			}
		})
	}
	if _, err := Resolve(&Flags{Format: "yaml"}, env(nil), Config{}, ""); err == nil {
		t.Error("bad format accepted")
	}
	if _, err := Resolve(&Flags{Level: "loud"}, env(nil), Config{}, ""); err == nil {
		t.Error("bad level accepted")
	}
	got, err := Resolve(&Flags{}, env(nil), Config{}, "/home/x/.forge")
	if err != nil {
		t.Fatal(err)
	}
	if got.File.Dir != "/home/x/.forge/logs" || got.File.MaxBytes != 50<<20 || got.File.MaxFiles != 5 {
		t.Errorf("file defaults = %+v", got.File)
	}
	if err := (Config{Level: "loud"}).Validate(); err == nil {
		t.Error("Validate accepted a bad level")
	}
	if err := (Config{Format: "yaml"}).Validate(); err == nil {
		t.Error("Validate accepted a bad format")
	}
	if err := (Config{Level: "debug,store=trace", Format: "json"}).Validate(); err != nil {
		t.Error(err)
	}
}

func TestAddFlagsParsesShorthands(t *testing.T) {
	fs := flag.NewFlagSet("t", flag.ContinueOnError)
	f := AddFlags(fs)
	if err := fs.Parse([]string{"-vv", "--log-level", "store=trace", "--log-format=json"}); err != nil {
		t.Fatal(err)
	}
	if !f.Trace || f.Level != "store=trace" || f.Format != "json" {
		t.Errorf("flags = %+v", *f)
	}
}

func TestHandlerComponentLevelsAndContextAttrs(t *testing.T) {
	var out bytes.Buffer
	h := New(&out, Options{Levels: Levels{Default: slog.LevelInfo, Components: map[string]slog.Level{"store": LevelTrace}}, Format: FormatJSON}, nil)
	store, worker := h.For("store"), h.For("worker")
	ctx := ContextWith(context.Background(), slog.String("attempt_id", "a1"), slog.String("work_id", "w1"))
	ctx = ContextWith(ctx, slog.String("attempt_id", "a2")) // later value wins

	store.Log(ctx, LevelTrace, "sql", "stmt", "select 1")
	worker.DebugContext(ctx, "hidden")
	worker.InfoContext(ctx, "shown")

	lines := strings.Split(strings.TrimSpace(out.String()), "\n")
	if len(lines) != 2 {
		t.Fatalf("got %d lines, want 2:\n%s", len(lines), out.String())
	}
	var first, second map[string]any
	if err := json.Unmarshal([]byte(lines[0]), &first); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal([]byte(lines[1]), &second); err != nil {
		t.Fatal(err)
	}
	if first["level"] != "TRACE" || first["component"] != "store" || first["attempt_id"] != "a2" || first["work_id"] != "w1" {
		t.Errorf("trace line = %v", first)
	}
	if second["component"] != "worker" || second["msg"] != "shown" || second["attempt_id"] != "a2" {
		t.Errorf("info line = %v", second)
	}
	if !HasAttr(ctx, "attempt_id") || HasAttr(ctx, "span_id") {
		t.Error("HasAttr wrong")
	}
	out.Reset()
	worker.WithGroup("req").With("k", "v").InfoContext(ctx, "grouped")
	var grouped map[string]any
	if err := json.Unmarshal(bytes.TrimSpace(out.Bytes()), &grouped); err != nil {
		t.Fatal(err)
	}
	if grouped["attempt_id"] != "a2" || grouped["k"] != "v" {
		t.Errorf("groups must stay flat: %v", grouped)
	}
	if worker.Enabled(ctx, slog.LevelDebug) || !store.Enabled(ctx, LevelTrace) {
		t.Error("Enabled disagrees with the sinks")
	}
}

func TestHandlerRuntimeLevelChangeAndToggle(t *testing.T) {
	var out bytes.Buffer
	h := New(&out, Options{Levels: Levels{Default: slog.LevelInfo}, Format: FormatText}, nil)
	l := h.For("x")
	l.Debug("one")
	h.SetLevels(Levels{Default: slog.LevelDebug})
	l.Debug("two")
	h.SetLevels(Levels{Default: slog.LevelWarn})
	h.ToggleDebug()
	l.Debug("three")
	h.ToggleDebug()
	l.Info("four")
	h.SetLevels(Levels{Default: slog.LevelInfo})
	h.ToggleDebug()
	h.SetLevels(Levels{Default: slog.LevelError}) // explicit set clears toggle memory
	if got := h.ToggleDebug(); got.Default != slog.LevelDebug {
		t.Errorf("toggle after explicit set = %v, want debug", got.Default)
	}
	h.SetLevels(Levels{Default: LevelTrace, Components: map[string]slog.Level{"store": slog.LevelError}})
	toggled := h.ToggleDebug()
	if toggled.Default != LevelTrace || toggled.Components["store"] != slog.LevelError {
		t.Errorf("toggle from trace = %v; want trace kept and components untouched", toggled)
	}
	toggled.Components["store"] = LevelTrace // a caller mutating its copy must not leak in
	if h.Levels().Components["store"] != slog.LevelError {
		t.Error("Levels() shares its map with callers")
	}
	if env := Environ(h); env[0] != "FORGE_LOG_LEVEL=trace,store=error" || env[1] != "FORGE_LOG_FORMAT=text" {
		t.Errorf("Environ = %v", env)
	}
	got := out.String()
	for _, want := range []string{"two", "three"} {
		if !strings.Contains(got, "msg="+want) {
			t.Errorf("missing %q in %q", want, got)
		}
	}
	for _, unwanted := range []string{"one", "four"} {
		if strings.Contains(got, "msg="+unwanted) {
			t.Errorf("unexpected %q in %q", unwanted, got)
		}
	}
}

func TestFileSinkAlwaysDebugAndRotates(t *testing.T) {
	dir := t.TempDir()
	sink, err := OpenFileSink("daemon", FileOptions{Dir: dir, MaxBytes: 400, MaxFiles: 2})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := sink.Close(); err != nil {
			t.Error(err)
		}
	}()
	var stderr bytes.Buffer
	h := New(&stderr, Options{Levels: Levels{Default: slog.LevelError}, Format: FormatText}, sink)
	l := h.For("store")
	for i := 0; i < 12; i++ {
		l.Debug("padding record to force rotation", "i", i, "filler", strings.Repeat("x", 60))
	}
	l.Log(context.Background(), LevelTrace, "never in file")
	if stderr.Len() != 0 {
		t.Errorf("stderr got %q at error level", stderr.String())
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	names := make([]string, 0, len(entries))
	for _, e := range entries {
		names = append(names, e.Name())
	}
	want := []string{"daemon.log", "daemon.log.1", "daemon.log.2"}
	if strings.Join(names, ",") != strings.Join(want, ",") {
		t.Errorf("files = %v, want %v", names, want)
	}
	data, err := os.ReadFile(filepath.Join(dir, "daemon.log"))
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(data, []byte("never in file")) {
		t.Error("trace record reached the file sink")
	}
	for _, name := range want {
		info, err := os.Stat(filepath.Join(dir, name))
		if err != nil {
			t.Fatal(err)
		}
		if info.Size() > 400+200 { // one record may overflow by its own length
			t.Errorf("%s is %d bytes", name, info.Size())
		}
		if info.Mode().Perm() != 0o600 {
			t.Errorf("%s mode = %v", name, info.Mode().Perm())
		}
	}
}

func TestFileSinkReopenLargeRecordAndMaxFilesEdges(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "w.log"), []byte(strings.Repeat("y", 100)), 0o600); err != nil {
		t.Fatal(err)
	}
	sink, err := OpenFileSink("w", FileOptions{Dir: dir, MaxBytes: 150, MaxFiles: 1})
	if err != nil {
		t.Fatal(err)
	}
	if sink.size != 100 {
		t.Errorf("size after reopen = %d, want 100", sink.size)
	}
	big := []byte(strings.Repeat("z", 400) + "\n") // larger than MaxBytes: rotates once, never loops
	if _, err := sink.Write(big); err != nil {
		t.Fatal(err)
	}
	if _, err := sink.Write([]byte("tail\n")); err != nil {
		t.Fatal(err)
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	for name, wantLen := range map[string]int{"w.log": 5, "w.log.1": 401} {
		data, err := os.ReadFile(filepath.Join(dir, name))
		if err != nil {
			t.Fatal(err)
		}
		if len(data) != wantLen {
			t.Errorf("%s has %d bytes, want %d", name, len(data), wantLen)
		}
	}
	if _, err := os.Stat(filepath.Join(dir, "w.log.2")); !os.IsNotExist(err) {
		t.Error("MaxFiles=1 kept a second generation")
	}

	zero, err := OpenFileSink("z", FileOptions{Dir: dir, MaxBytes: 10, MaxFiles: 0})
	if err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 5; i++ {
		if _, err := zero.Write([]byte("0123456789\n")); err != nil {
			t.Fatal(err)
		}
	}
	if err := zero.Close(); err != nil {
		t.Fatal(err)
	}
	if info, err := os.Stat(filepath.Join(dir, "z.log")); err != nil || info.Size() != 11 {
		t.Errorf("MaxFiles=0 should truncate: %v %v", info, err)
	}
	if _, err := os.Stat(filepath.Join(dir, "z.log.1")); !os.IsNotExist(err) {
		t.Error("MaxFiles=0 kept a generation")
	}
}

func TestFileSinkConcurrentWriters(t *testing.T) {
	dir := t.TempDir()
	sink, err := OpenFileSink("c", FileOptions{Dir: dir, MaxBytes: 2000, MaxFiles: 3})
	if err != nil {
		t.Fatal(err)
	}
	h := New(io.Discard, Options{Levels: Levels{Default: slog.LevelError}, Format: FormatText}, sink)
	var wg sync.WaitGroup
	for g := 0; g < 8; g++ {
		wg.Add(1)
		go func(g int) {
			defer wg.Done()
			l := h.For("w")
			for i := 0; i < 50; i++ {
				l.Debug("record", "g", g, "i", i)
			}
		}(g)
	}
	wg.Wait()
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	total := 0
	for _, name := range []string{"c.log", "c.log.1", "c.log.2", "c.log.3"} {
		data, err := os.ReadFile(filepath.Join(dir, name))
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			t.Fatal(err)
		}
		for _, line := range bytes.Split(bytes.TrimSpace(data), []byte("\n")) {
			var rec map[string]any
			if err := json.Unmarshal(line, &rec); err != nil {
				t.Fatalf("interleaved or torn record in %s: %q", name, line)
			}
			total++
		}
	}
	if total == 0 || total > 400 {
		t.Errorf("recovered %d records", total)
	}
}

func TestForwardLongLineAndChildAttrs(t *testing.T) {
	var out bytes.Buffer
	h := New(&out, Options{Levels: Levels{Default: LevelTrace}, Format: FormatJSON}, nil)
	long := strings.Repeat("p", maxForwardLine+10)
	child := long + "\nAFTER\n" + `{"time":"t","level":"INFO","msg":"m","component":"worker.git","level_nested":{"level":"x"}}` + "\n"
	if err := Forward(context.Background(), strings.NewReader(child), h.For("worker")); err != nil {
		t.Fatal(err)
	}
	lines := strings.Split(strings.TrimSpace(out.String()), "\n")
	if len(lines) != 4 {
		t.Fatalf("got %d lines, want 4 (two chunks, AFTER, record)", len(lines))
	}
	if !strings.Contains(lines[0], `"truncated":true`) || !strings.Contains(lines[2], "AFTER") {
		t.Errorf("long line handling: %v", lines[:3])
	}
	var rec map[string]any
	if err := json.Unmarshal([]byte(lines[3]), &rec); err != nil {
		t.Fatal(err)
	}
	if rec["component"] != "worker" || rec["child_component"] != "worker.git" || strings.Count(lines[3], `"component"`) != 1 {
		t.Errorf("component collision: %s", lines[3])
	}
	pr, pw := io.Pipe()
	go func() { _ = pw.CloseWithError(io.ErrUnexpectedEOF) }()
	if err := Forward(context.Background(), pr, h.For("worker")); err != io.ErrUnexpectedEOF {
		t.Errorf("read error not propagated: %v", err)
	}
}

func TestForwardChildStderr(t *testing.T) {
	var out bytes.Buffer
	h := New(&out, Options{Levels: Levels{Default: LevelTrace}, Format: FormatJSON}, nil)
	child := `{"time":"2026-08-30T00:00:00Z","level":"DEBUG","msg":"claimed","attempt_id":"a1"}
panic: something broke
{"not":"a record"}
`
	if err := Forward(context.Background(), strings.NewReader(child), h.For("worker")); err != nil {
		t.Fatal(err)
	}
	lines := strings.Split(strings.TrimSpace(out.String()), "\n")
	if len(lines) != 3 {
		t.Fatalf("got %d lines: %s", len(lines), out.String())
	}
	var rec map[string]any
	if err := json.Unmarshal([]byte(lines[0]), &rec); err != nil {
		t.Fatal(err)
	}
	if rec["level"] != "DEBUG" || rec["msg"] != "claimed" || rec["attempt_id"] != "a1" || rec["component"] != "worker" {
		t.Errorf("forwarded record = %v", rec)
	}
	if !strings.Contains(lines[1], `"level":"WARN"`) || !strings.Contains(lines[1], "panic: something broke") {
		t.Errorf("plain line = %s", lines[1])
	}
	if !strings.Contains(lines[2], `"level":"WARN"`) {
		t.Errorf("non-record JSON line = %s", lines[2])
	}
}

func TestHandleSIGUSR1TogglesDebug(t *testing.T) {
	var out bytes.Buffer
	h := New(&out, Options{Levels: Levels{Default: slog.LevelInfo}, Format: FormatText}, nil)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	changed := make(chan Levels, 1)
	w := WatchSIGUSR1(h, h.For("main"), func(l Levels) { changed <- l }) // registered before any signal is sent
	done := make(chan struct{})
	go func() {
		defer close(done)
		w.Run(ctx)
	}()
	waitToggle := func(want slog.Level) {
		t.Helper()
		if err := syscall.Kill(os.Getpid(), syscall.SIGUSR1); err != nil {
			t.Fatal(err)
		}
		select {
		case l := <-changed:
			if l.Default != want {
				t.Errorf("after SIGUSR1 default = %v, want %v", l.Default, want)
			}
		case <-time.After(5 * time.Second):
			t.Fatal("SIGUSR1 never toggled")
		}
	}
	waitToggle(slog.LevelDebug)
	waitToggle(slog.LevelInfo)
	cancel()
	<-done
	if !strings.Contains(out.String(), "trigger=SIGUSR1") {
		t.Error("level change not logged")
	}
}
