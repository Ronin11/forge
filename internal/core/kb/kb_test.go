package kb

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

var now = time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)

func TestWriteNewParseRoundTrip(t *testing.T) {
	dir := t.TempDir()
	n, err := WriteNew(dir, New{
		Title: "Smoke: a Note!", Type: "note", Tags: []string{"smoke", "test"},
		Links: map[string][]string{"about": {"attempt:" + strings.Repeat("a", 32), "other-note"}},
		Body:  "Sees [[other-note]] and [[missing-one]].\n",
	}, now)
	if err != nil {
		t.Fatal(err)
	}
	if n.ID != "smoke-a-note" || n.Type != "note" || n.Title != "Smoke: a Note!" || !n.Created.Equal(now) {
		t.Errorf("note = %+v", n)
	}
	if len(n.Links["about"]) != 2 || n.Links["about"][0] != "attempt:"+strings.Repeat("a", 32) {
		t.Errorf("links = %v", n.Links)
	}
	if len(n.Inline) != 2 || n.Inline[0] != "other-note" || n.Inline[1] != "missing-one" {
		t.Errorf("inline = %v", n.Inline)
	}
	info, err := os.Stat(n.Path)
	if err != nil || info.Mode().Perm() != 0o600 {
		t.Errorf("mode: %v %v", info, err)
	}
	if _, err := WriteNew(dir, New{Title: "Smoke: a note"}, now); err == nil {
		t.Error("collision accepted")
	}
	for _, bad := range []New{
		{Title: "x", Type: "bogus"},
		{Title: ""},
		{Title: "x2", Links: map[string][]string{"about": {"Not A Ref"}}},
		{Title: "x3", Links: map[string][]string{"nearby": {"other"}}},
		{Title: "x4", Links: map[string][]string{"about": {"attempt:short"}}},
	} {
		if _, err := WriteNew(dir, bad, now); err == nil {
			t.Errorf("accepted %+v", bad)
		}
	}
}

func TestParseRef(t *testing.T) {
	good := map[string]string{
		"some-note": "note", "attempt:" + strings.Repeat("1", 32): "attempt",
		"routine:inventory": "routine", "routine:inventory@3": "routine",
		"repository:equitizr": "repository", "prompt:" + strings.Repeat("ab", 32): "prompt",
	}
	for s, kind := range good {
		ref, err := ParseRef(s)
		if err != nil || ref.Kind != kind {
			t.Errorf("ParseRef(%q) = %+v, %v", s, ref, err)
		}
	}
	for _, bad := range []string{"attempt:xyz", "routine:Bad Name", "prompt:1234", "UPPER", "a..b", "unknown:thing"} {
		if _, err := ParseRef(bad); err == nil {
			t.Errorf("ParseRef(%q) accepted", bad)
		}
	}
}

func TestCheckFindsEveryProblem(t *testing.T) {
	dir := t.TempDir()
	if _, err := WriteNew(dir, New{Title: "Alpha", Body: "links [[beta]]\n"}, now); err != nil {
		t.Fatal(err)
	}
	if _, err := WriteNew(dir, New{Title: "Beta", Body: "fine\n"}, now); err != nil {
		t.Fatal(err)
	}
	res, err := Check(dir, nil)
	if err != nil || len(res.Findings) != 0 || res.Notes != 2 {
		t.Fatalf("clean kb: %+v, %v", res, err)
	}
	// Dangling note link (smoke 12's shape).
	appendTo(t, filepath.Join(dir, "alpha.md"), "\nsee [[does-not-exist]]\n")
	res, err = Check(dir, nil)
	if err != nil || len(res.Findings) != 1 || !strings.Contains(res.Findings[0].Problem, "does-not-exist") {
		t.Fatalf("dangling: %+v, %v", res, err)
	}
	// Mismatched id vs filename.
	writeRaw(t, filepath.Join(dir, "gamma.md"), "---\nid: delta\ntitle: \"G\"\ntype: note\ncreated: 2026-08-30T12:00:00Z\n---\nbody\n")
	// Malformed frontmatter.
	writeRaw(t, filepath.Join(dir, "eps.md"), "no frontmatter at all\n")
	// A second file claiming an existing id fails the stem rule, which is what
	// makes duplicate ids impossible.
	writeRaw(t, filepath.Join(dir, "alpha2.md"), "---\nid: alpha\ntitle: \"A2\"\ntype: note\ncreated: 2026-08-30T12:00:00Z\n---\nbody\n")
	res, err = Check(dir, nil)
	if err != nil {
		t.Fatal(err)
	}
	problems := strings.Builder{}
	for _, f := range res.Findings {
		problems.WriteString(f.Problem + "\n")
	}
	for _, want := range []string{"does-not-exist", "does not match the filename stem", "missing frontmatter", `id "alpha" does not match`} {
		if !strings.Contains(problems.String(), want) {
			t.Errorf("missing finding %q in:\n%s", want, problems.String())
		}
	}
	// Fact links: verified, dangling, and unverifiable.
	facts := func(ref Ref) (bool, error) {
		if ref.Kind == "attempt" && ref.Val == strings.Repeat("a", 32) {
			return true, nil
		}
		return false, nil
	}
	if _, err := WriteNew(dir, New{Title: "Facts", Links: map[string][]string{"about": {"attempt:" + strings.Repeat("a", 32), "attempt:" + strings.Repeat("b", 32)}}}, now); err != nil {
		t.Fatal(err)
	}
	res, err = Check(dir, facts)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, f := range res.Findings {
		if strings.Contains(f.Problem, "dangling fact link attempt:bbbb") {
			found = true
		}
	}
	if !found {
		t.Errorf("dangling fact link not reported: %+v", res.Findings)
	}
	res, err = Check(dir, func(Ref) (bool, error) { return false, os.ErrDeadlineExceeded })
	if err != nil || len(res.Warnings) == 0 {
		t.Errorf("unverifiable facts must warn, not fail: %+v, %v", res, err)
	}
}

func TestScanExportAndSlug(t *testing.T) {
	dir := t.TempDir()
	if _, err := WriteNew(dir, New{Title: "One", Body: "see [[two]] and attempt link [[two]] again\n"}, now); err != nil {
		t.Fatal(err)
	}
	if _, err := WriteNew(dir, New{Title: "Two", Body: "plain\n"}, now); err != nil {
		t.Fatal(err)
	}
	out := t.TempDir()
	n, err := Export(dir, out)
	if err != nil || n != 2 {
		t.Fatalf("export: %d, %v", n, err)
	}
	data, err := os.ReadFile(filepath.Join(out, "one.md"))
	if err != nil || !strings.Contains(string(data), "[two](two.md)") {
		t.Errorf("export rewrite: %q %v", data, err)
	}
	orig, err := os.ReadFile(filepath.Join(dir, "one.md"))
	if err != nil || !strings.Contains(string(orig), "[[two]]") {
		t.Errorf("canonical file was rewritten: %q %v", orig, err)
	}
	if Slug("Hello, World! — again") != "hello-world-again" || Slug("--x--") != "x" {
		t.Errorf("slug: %q %q", Slug("Hello, World! — again"), Slug("--x--"))
	}
	notes, findings, err := Scan(t.TempDir() + "/absent")
	if err != nil || notes != nil || findings != nil {
		t.Errorf("absent dir: %v %v %v", notes, findings, err)
	}
}

func appendTo(t *testing.T, path, s string) {
	t.Helper()
	f, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteString(s); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
}

func writeRaw(t *testing.T, path, s string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(s), 0o600); err != nil {
		t.Fatal(err)
	}
}
