// Package kb is the knowledge base: Markdown notes with typed frontmatter and
// [[id]] links, indexed in the daemon's SQLite for search and backlinks. The
// files are the truth; the index is derived and rebuilt incrementally. This
// package owns parsing, validation, file writes, and the integrity check; it
// has no database code (that lives in store) and no HTTP.
package kb

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"
)

// Note types (DESIGN.md §11).
var NoteTypes = map[string]bool{"retro": true, "hypothesis": true, "proposal": true, "spec": true, "note": true}

// Link types for frontmatter links; inline [[id]] links are "inline".
var LinkTypes = []string{"about", "supersedes", "evidence_for"}

// idPattern is the note id grammar: like names, but longer (a slug of a title).
var idPattern = regexp.MustCompile(`^[a-z0-9][a-z0-9-]{0,79}$`)

// factRefPattern is the fact-link grammar of DESIGN.md §2.
var factRefPattern = regexp.MustCompile(`^(attempt|work|target|proposal):([0-9a-f]{32})$|^(routine):([a-z0-9][a-z0-9-]{0,39})(@[0-9]+)?$|^(repository|project):([a-z0-9][a-z0-9-]{0,39})$|^(prompt):([0-9a-f]{64})$`)

// inlinePattern finds [[id]] links in a body.
var inlinePattern = regexp.MustCompile(`\[\[([^\[\]|]+)\]\]`)

// Note is one parsed file.
type Note struct {
	ID      string
	Path    string
	Title   string
	Type    string
	Created time.Time
	Tags    []string
	// Links maps link type (about, supersedes, evidence_for) to targets; each
	// target is a note id or a fact ref.
	Links map[string][]string
	// Inline are the [[id]] targets found in the body, in order, unique.
	Inline []string
	Body   string
	// ModTime and Hash drive incremental reindexing.
	ModTime time.Time
	Size    int64
}

// Ref is a parsed link target: either a note id or a fact reference.
type Ref struct {
	Kind string // "note", or the fact kind: attempt, work, target, routine, repository, project, proposal, prompt
	Val  string // note id, hex id, or name (routine may carry "@N")
}

// ParseRef classifies a link target. Anything with a known "kind:" prefix must
// match the fact grammar exactly; anything else must be a note id.
func ParseRef(s string) (Ref, error) {
	if k, _, ok := strings.Cut(s, ":"); ok {
		switch k {
		case "attempt", "work", "target", "routine", "repository", "project", "proposal", "prompt":
			if !factRefPattern.MatchString(s) {
				return Ref{}, fmt.Errorf("malformed fact link %q", s)
			}
			return Ref{Kind: k, Val: strings.TrimPrefix(s, k+":")}, nil
		}
	}
	if !idPattern.MatchString(s) {
		return Ref{}, fmt.Errorf("malformed link target %q: not a note id or fact link", s)
	}
	return Ref{Kind: "note", Val: s}, nil
}

// Slug derives a note id from a title; the caller must check for collisions.
func Slug(title string) string {
	var b strings.Builder
	dash := false
	for _, r := range strings.ToLower(title) {
		switch {
		case r >= 'a' && r <= 'z' || r >= '0' && r <= '9':
			b.WriteRune(r)
			dash = false
		default:
			if !dash && b.Len() > 0 {
				b.WriteByte('-')
				dash = true
			}
		}
	}
	s := strings.Trim(b.String(), "-")
	if len(s) > 80 {
		s = strings.Trim(s[:80], "-")
	}
	return s
}

// Parse reads one note file. Errors name the file and the exact problem so
// `forge kb check` output is actionable.
func Parse(path string) (*Note, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	info, err := os.Stat(path)
	if err != nil {
		return nil, fmt.Errorf("stat %s: %w", path, err)
	}
	n := &Note{Path: path, Links: map[string][]string{}, ModTime: info.ModTime(), Size: info.Size()}
	content := string(raw)
	front, body, err := splitFrontmatter(content)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	n.Body = body
	if err := n.parseFrontmatter(front); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	stem := strings.TrimSuffix(filepath.Base(path), ".md")
	if n.ID != stem {
		return nil, fmt.Errorf("%s: id %q does not match the filename stem %q", path, n.ID, stem)
	}
	seen := map[string]bool{}
	for _, m := range inlinePattern.FindAllStringSubmatch(body, -1) {
		t := strings.TrimSpace(m[1])
		if !seen[t] {
			seen[t] = true
			n.Inline = append(n.Inline, t)
		}
	}
	return n, nil
}

// splitFrontmatter separates the --- fenced YAML-ish header from the body.
func splitFrontmatter(content string) (front []string, body string, err error) {
	if !strings.HasPrefix(content, "---\n") {
		return nil, "", fmt.Errorf("missing frontmatter (--- fence)")
	}
	rest := content[4:]
	end := strings.Index(rest, "\n---")
	if end < 0 {
		return nil, "", fmt.Errorf("unterminated frontmatter")
	}
	body = strings.TrimPrefix(rest[end+4:], "\n")
	return strings.Split(rest[:end], "\n"), body, nil
}

// parseFrontmatter reads the small fixed schema. It is not a YAML parser on
// purpose: the format is exactly what WriteNew writes, and anything else is a
// check failure, not something to guess about.
func (n *Note) parseFrontmatter(lines []string) error {
	section := ""
	for _, line := range lines {
		if strings.TrimSpace(line) == "" {
			continue
		}
		indented := strings.HasPrefix(line, "  ")
		line = strings.TrimSpace(line)
		if indented && section == "links" {
			key, val, ok := strings.Cut(line, ":")
			if !ok {
				return fmt.Errorf("frontmatter: malformed links entry %q", line)
			}
			key = strings.TrimSpace(key)
			if key != "about" && key != "supersedes" && key != "evidence_for" {
				return fmt.Errorf("frontmatter: unknown link type %q", key)
			}
			targets, err := parseList(val)
			if err != nil {
				return fmt.Errorf("frontmatter: links.%s: %w", key, err)
			}
			n.Links[key] = targets
			continue
		}
		section = ""
		key, val, ok := strings.Cut(line, ":")
		if !ok {
			return fmt.Errorf("frontmatter: malformed line %q", line)
		}
		key, val = strings.TrimSpace(key), strings.TrimSpace(val)
		switch key {
		case "id":
			n.ID = val
		case "title":
			n.Title = strings.Trim(val, `"`)
		case "type":
			n.Type = val
		case "created":
			t, err := time.Parse(time.RFC3339, val)
			if err != nil {
				return fmt.Errorf("frontmatter: created: %w", err)
			}
			n.Created = t
		case "tags":
			tags, err := parseList(val)
			if err != nil {
				return fmt.Errorf("frontmatter: tags: %w", err)
			}
			n.Tags = tags
		case "links":
			section = "links"
		default:
			return fmt.Errorf("frontmatter: unknown key %q", key)
		}
	}
	switch {
	case n.ID == "":
		return fmt.Errorf("frontmatter: id is required")
	case !idPattern.MatchString(n.ID):
		return fmt.Errorf("frontmatter: id %q is not a valid note id", n.ID)
	case n.Title == "":
		return fmt.Errorf("frontmatter: title is required")
	case !NoteTypes[n.Type]:
		return fmt.Errorf("frontmatter: type %q is not retro|hypothesis|proposal|spec|note", n.Type)
	case n.Created.IsZero():
		return fmt.Errorf("frontmatter: created is required")
	}
	return nil
}

func parseList(val string) ([]string, error) {
	val = strings.TrimSpace(val)
	if val == "" || val == "[]" {
		return nil, nil
	}
	if !strings.HasPrefix(val, "[") || !strings.HasSuffix(val, "]") {
		return nil, fmt.Errorf("want [a, b, …], got %q", val)
	}
	var out []string
	for _, item := range strings.Split(val[1:len(val)-1], ",") {
		item = strings.Trim(strings.TrimSpace(item), `"`)
		if item != "" {
			out = append(out, item)
		}
	}
	return out, nil
}

// New is the input to WriteNew.
type New struct {
	Title string
	Type  string
	Tags  []string
	Links map[string][]string // about/supersedes/evidence_for → targets
	Body  string
	ID    string // optional; derived from Title otherwise
}

// WriteNew creates a note file (0600) and returns the parsed note. It refuses
// collisions and validates every link target's grammar — agents create notes
// only through this path, so a malformed link never reaches disk.
func WriteNew(dir string, in New, now time.Time) (*Note, error) {
	id := in.ID
	if id == "" {
		id = Slug(in.Title)
	}
	if !idPattern.MatchString(id) {
		return nil, fmt.Errorf("id %q is not a valid note id", id)
	}
	if in.Type == "" {
		in.Type = "note"
	}
	if !NoteTypes[in.Type] {
		return nil, fmt.Errorf("type %q is not retro|hypothesis|proposal|spec|note", in.Type)
	}
	if in.Title == "" {
		return nil, fmt.Errorf("title is required")
	}
	for lt, targets := range in.Links {
		ok := false
		for _, known := range LinkTypes {
			if lt == known {
				ok = true
			}
		}
		if !ok {
			return nil, fmt.Errorf("unknown link type %q", lt)
		}
		for _, t := range targets {
			if _, err := ParseRef(t); err != nil {
				return nil, err
			}
		}
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("create kb dir: %w", err)
	}
	path := filepath.Join(dir, id+".md")
	if _, err := os.Stat(path); err == nil {
		return nil, fmt.Errorf("note %s already exists", id)
	} else if !os.IsNotExist(err) {
		return nil, fmt.Errorf("stat %s: %w", path, err)
	}
	var b strings.Builder
	fmt.Fprintf(&b, "---\nid: %s\ntitle: %q\ntype: %s\ncreated: %s\n", id, in.Title, in.Type, now.UTC().Format(time.RFC3339))
	if len(in.Tags) > 0 {
		fmt.Fprintf(&b, "tags: [%s]\n", strings.Join(in.Tags, ", "))
	}
	if len(in.Links) > 0 {
		b.WriteString("links:\n")
		for _, lt := range LinkTypes {
			if targets := in.Links[lt]; len(targets) > 0 {
				fmt.Fprintf(&b, "  %s: [%s]\n", lt, strings.Join(targets, ", "))
			}
		}
	}
	b.WriteString("---\n\n")
	b.WriteString(in.Body)
	if !strings.HasSuffix(in.Body, "\n") {
		b.WriteString("\n")
	}
	if err := os.WriteFile(path, []byte(b.String()), 0o600); err != nil {
		return nil, fmt.Errorf("write %s: %w", path, err)
	}
	return Parse(path)
}

// Scan parses every .md file under dir (flat; subdirectories are build output
// or attachments and are ignored). Unparseable files are returned as findings,
// not errors: one bad note must not hide the rest.
func Scan(dir string) (notes []*Note, findings []Finding, err error) {
	entries, err := os.ReadDir(dir)
	if os.IsNotExist(err) {
		return nil, nil, nil
	}
	if err != nil {
		return nil, nil, fmt.Errorf("read kb dir %s: %w", dir, err)
	}
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
			continue
		}
		path := filepath.Join(dir, e.Name())
		n, perr := Parse(path)
		if perr != nil {
			findings = append(findings, Finding{Path: path, Problem: perr.Error()})
			continue
		}
		// Duplicate ids are structurally impossible: an id must equal its
		// filename stem, and stems are unique within the directory.
		notes = append(notes, n)
	}
	sort.Slice(notes, func(i, j int) bool { return notes[i].ID < notes[j].ID })
	return notes, findings, nil
}

// Finding is one integrity problem, named precisely.
type Finding struct {
	Path    string
	Problem string
}

// FactChecker reports whether a fact reference resolves; nil means "cannot
// verify" (the daemon is down), which Check reports as a warning, not a failure.
type FactChecker func(ref Ref) (exists bool, err error)

// CheckResult is what `forge kb check` prints and exits on.
type CheckResult struct {
	Notes    int
	Findings []Finding
	Warnings []string
}

// Check validates the whole kb: parseable frontmatter, unique matching ids,
// well-formed links, no dangling note links, and — when facts can be checked —
// no dangling fact links. It runs in `just check`, so it must work offline.
func Check(dir string, facts FactChecker) (*CheckResult, error) {
	notes, findings, err := Scan(dir)
	if err != nil {
		return nil, err
	}
	res := &CheckResult{Notes: len(notes), Findings: findings}
	ids := map[string]bool{}
	for _, n := range notes {
		ids[n.ID] = true
	}
	for _, n := range notes {
		refs := map[string][]string{"inline": n.Inline}
		for lt, targets := range n.Links {
			refs[lt] = targets
		}
		for lt, targets := range refs {
			for _, t := range targets {
				ref, err := ParseRef(t)
				if err != nil {
					res.Findings = append(res.Findings, Finding{Path: n.Path, Problem: fmt.Sprintf("%s link: %v", lt, err)})
					continue
				}
				if ref.Kind == "note" {
					if !ids[ref.Val] {
						res.Findings = append(res.Findings, Finding{Path: n.Path, Problem: fmt.Sprintf("dangling note link [[%s]]", ref.Val)})
					}
					continue
				}
				if facts == nil {
					continue
				}
				exists, err := facts(ref)
				if err != nil {
					res.Warnings = append(res.Warnings, fmt.Sprintf("%s: fact link %s unverified: %v", n.Path, t, err))
					continue
				}
				if !exists {
					res.Findings = append(res.Findings, Finding{Path: n.Path, Problem: fmt.Sprintf("dangling fact link %s", t)})
				}
			}
		}
	}
	return res, nil
}

// Export rewrites [[id]] links to relative Markdown links into outDir; the
// canonical files are never touched.
func Export(dir, outDir string) (int, error) {
	notes, findings, err := Scan(dir)
	if err != nil {
		return 0, err
	}
	if len(findings) > 0 {
		return 0, fmt.Errorf("kb has %d problems; run `forge kb check` first", len(findings))
	}
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		return 0, fmt.Errorf("create %s: %w", outDir, err)
	}
	for _, n := range notes {
		raw, err := os.ReadFile(n.Path)
		if err != nil {
			return 0, fmt.Errorf("read %s: %w", n.Path, err)
		}
		out := inlinePattern.ReplaceAllStringFunc(string(raw), func(m string) string {
			id := strings.TrimSpace(m[2 : len(m)-2])
			if ref, err := ParseRef(id); err == nil && ref.Kind == "note" {
				return fmt.Sprintf("[%s](%s.md)", id, id)
			}
			return m
		})
		if err := os.WriteFile(filepath.Join(outDir, n.ID+".md"), []byte(out), 0o644); err != nil {
			return 0, fmt.Errorf("write export: %w", err)
		}
	}
	return len(notes), nil
}
