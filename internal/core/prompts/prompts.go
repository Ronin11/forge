// Package prompts is the file-backed persona and template library: Markdown
// under a git-versioned directory (default ~/.forge/prompts), loaded and
// validated by the daemon, composed into routine prompts at Work creation.
// Git owns authoring — diffs, blame, revert, and agents editing prompts
// through the ordinary branch-and-merge pipeline; the daemon owns reading:
// it keeps the last good tree if a load fails, and every run freezes the
// resolved bytes plus a composition manifest (fragment hashes and the
// library's commit), so "what ran" never depends on the working tree.
//
// The layout is personas/*.md and fragments/**.md. A file is optional
// frontmatter (--- fenced, `model: <alias>` for personas) plus a body. The
// composition language is deliberately dumb: `{{> name}}` includes a
// fragment, `{{> name key="value"}}` passes parameters substituted for
// `{{key}}` inside that fragment only, and a persona body may carry
// `## mode: <name>` sections composed only into runs of that mode. No
// conditionals, no loops — teaching is selection, not branching. Unknown
// includes, cycles, and over-deep nesting are load-time errors, so a broken
// edit is refused before any run sees it. Placeholders the library does not
// own ({{objective}}, {{repo}}) pass through untouched for the existing
// substitutions downstream.
package prompts

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"
)

// MaxIncludeDepth bounds nesting; a tree deeper than this is a maze, not a
// library.
const MaxIncludeDepth = 8

// MaxResolvedBytes caps one resolved persona; past this the prompt is doing a
// document's job.
const MaxResolvedBytes = 256 * 1024

// Fragment is one file: a fragment proper, or a persona (personas are
// fragments that live under personas/ and may declare a model and mode
// sections).
type Fragment struct {
	Name    string `json:"name"` // relative path minus .md: "escalation-rules", "house-style/go"
	Path    string `json:"path"`
	Model   string `json:"model,omitempty"` // personas: default model alias, routine overrides
	Body    string `json:"-"`               // core body, mode sections split out
	Modes   map[string]string
	Hash    string `json:"hash"` // sha256 of the raw file
	Persona bool   `json:"persona"`
}

// Library is one loaded, validated tree. It is immutable once built — the
// daemon swaps whole libraries, never mutates one.
type Library struct {
	Dir       string
	Commit    string // git HEAD of the prompts dir; "" when not a repo
	Dirty     bool   // uncommitted changes at load time
	LoadedAt  time.Time
	fragments map[string]*Fragment
}

// ManifestEntry records one fragment that went into a resolved prompt.
type ManifestEntry struct {
	Name string `json:"name"`
	Hash string `json:"hash"`
}

// Composition is the audit record frozen with a Work: exactly which fragment
// bytes, from which library state, composed its prompt.
type Composition struct {
	Persona   string          `json:"persona"`
	Mode      string          `json:"mode,omitempty"`
	Commit    string          `json:"commit,omitempty"`
	Dirty     bool            `json:"dirty,omitempty"`
	Fragments []ManifestEntry `json:"fragments"`
}

var (
	nameSegment = regexp.MustCompile(`^[a-z0-9][a-z0-9-]{0,63}$`)
	includeRef  = regexp.MustCompile(`\{\{>\s*([a-z0-9/-]+)((?:\s+[a-z0-9-]+="[^"]*")*)\s*\}\}`)
	includeArg  = regexp.MustCompile(`([a-z0-9-]+)="([^"]*)"`)
	modeHeading = regexp.MustCompile(`(?m)^## mode:[ \t]*([a-z0-9-]+)[ \t]*$`)
)

// ValidateName checks a fragment name: slash-separated slug segments.
func ValidateName(name string) error {
	if name == "" {
		return fmt.Errorf("empty fragment name")
	}
	for _, seg := range strings.Split(name, "/") {
		if !nameSegment.MatchString(seg) {
			return fmt.Errorf("fragment name %q: segment %q is not a lower-case slug", name, seg)
		}
	}
	return nil
}

// Load reads and validates the whole tree. Any error means the caller keeps
// its previous library: a broken edit must never brick run creation.
func Load(dir string) (*Library, error) {
	lib := &Library{Dir: dir, LoadedAt: time.Now().UTC(), fragments: map[string]*Fragment{}}
	for _, sub := range []string{"personas", "fragments"} {
		root := filepath.Join(dir, sub)
		err := filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
			if err != nil {
				if os.IsNotExist(err) && path == root {
					return filepath.SkipDir
				}
				return err
			}
			if d.IsDir() || !strings.HasSuffix(path, ".md") {
				return nil
			}
			rel, err := filepath.Rel(root, path)
			if err != nil {
				return err
			}
			f, err := loadFragment(path, filepath.ToSlash(strings.TrimSuffix(rel, ".md")), sub == "personas")
			if err != nil {
				return err
			}
			if dup, ok := lib.fragments[f.Name]; ok {
				return fmt.Errorf("fragment %q defined twice (%s and %s)", f.Name, dup.Path, f.Path)
			}
			lib.fragments[f.Name] = f
			return nil
		})
		if err != nil && !os.IsNotExist(err) {
			return nil, err
		}
	}
	lib.Commit, lib.Dirty = gitState(dir)
	// Validate every composition path now: each fragment alone, and each
	// persona's core plus each of its mode sections.
	for _, f := range lib.fragments {
		if _, _, err := lib.expand(f, "", nil, 0); err != nil {
			return nil, fmt.Errorf("%s: %w", f.Path, err)
		}
		for mode := range f.Modes {
			if _, _, err := lib.expand(f, mode, nil, 0); err != nil {
				return nil, fmt.Errorf("%s (mode %s): %w", f.Path, mode, err)
			}
		}
	}
	return lib, nil
}

func loadFragment(path, name string, persona bool) (*Fragment, error) {
	if err := ValidateName(name); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	sum := sha256.Sum256(raw)
	f := &Fragment{Name: name, Path: path, Hash: hex.EncodeToString(sum[:]), Persona: persona, Modes: map[string]string{}}
	body := string(raw)
	// Frontmatter is optional: a bare Markdown file is a fine fragment.
	if strings.HasPrefix(body, "---\n") {
		rest := body[4:]
		end := strings.Index(rest, "\n---")
		if end < 0 {
			return nil, fmt.Errorf("%s: unterminated frontmatter", path)
		}
		for _, line := range strings.Split(rest[:end], "\n") {
			line = strings.TrimSpace(line)
			if line == "" || strings.HasPrefix(line, "#") {
				continue
			}
			key, val, ok := strings.Cut(line, ":")
			if !ok {
				return nil, fmt.Errorf("%s: frontmatter line %q", path, line)
			}
			switch strings.TrimSpace(key) {
			case "model":
				f.Model = strings.TrimSpace(val)
			default:
				return nil, fmt.Errorf("%s: unknown frontmatter key %q", path, strings.TrimSpace(key))
			}
		}
		body = strings.TrimPrefix(rest[end+4:], "\n")
	}
	f.Body, f.Modes = splitModeSections(body)
	return f, nil
}

// splitModeSections carves `## mode: <name>` sections out of a body: the core
// is everything before the first heading, each section runs to the next.
func splitModeSections(body string) (core string, modes map[string]string) {
	modes = map[string]string{}
	locs := modeHeading.FindAllStringSubmatchIndex(body, -1)
	if len(locs) == 0 {
		return strings.TrimSpace(body), modes
	}
	core = strings.TrimSpace(body[:locs[0][0]])
	for i, loc := range locs {
		end := len(body)
		if i+1 < len(locs) {
			end = locs[i+1][0]
		}
		modes[body[loc[2]:loc[3]]] = strings.TrimSpace(body[loc[1]:end])
	}
	return core, modes
}

// Persona returns a persona by name, or nil.
func (l *Library) Persona(name string) *Fragment {
	f := l.fragments[name]
	if f == nil || !f.Persona {
		return nil
	}
	return f
}

// Fragments lists every fragment, personas included, sorted by name.
func (l *Library) Fragments() []*Fragment {
	out := make([]*Fragment, 0, len(l.fragments))
	for _, f := range l.fragments {
		out = append(out, f)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// Resolve composes a persona for a mode: the fully expanded text and the
// manifest of every fragment that went in. mode selects the matching
// `## mode:` section when the persona has one; an unknown mode simply gets
// the core — modes vary per routine and a persona need not know them all.
func (l *Library) Resolve(persona, mode string) (string, Composition, error) {
	f := l.Persona(persona)
	if f == nil {
		return "", Composition{}, fmt.Errorf("persona %q is not in the library (personas/%s.md)", persona, persona)
	}
	used := map[string]bool{}
	text, names, err := l.expand(f, mode, used, 0)
	if err != nil {
		return "", Composition{}, err
	}
	comp := Composition{Persona: persona, Mode: mode, Commit: l.Commit, Dirty: l.Dirty}
	for _, n := range names {
		comp.Fragments = append(comp.Fragments, ManifestEntry{Name: n, Hash: l.fragments[n].Hash})
	}
	return text, comp, nil
}

// expand resolves one fragment's text (core plus the mode section for the
// root), recursing through includes. used collects fragment names across the
// whole expansion (nil = validation only); the returned names are sorted.
func (l *Library) expand(f *Fragment, mode string, used map[string]bool, depth int) (string, []string, error) {
	if depth > MaxIncludeDepth {
		return "", nil, fmt.Errorf("includes nest deeper than %d at %q", MaxIncludeDepth, f.Name)
	}
	if used == nil {
		used = map[string]bool{}
	}
	used[f.Name] = true
	text := f.Body
	if section, ok := f.Modes[mode]; ok && mode != "" {
		text = strings.TrimSpace(text + "\n\n" + section)
	}
	expanded, err := l.expandIncludes(text, used, depth, map[string]bool{f.Name: true})
	if err != nil {
		return "", nil, fmt.Errorf("in %q: %w", f.Name, err)
	}
	if len(expanded) > MaxResolvedBytes {
		return "", nil, fmt.Errorf("%q resolves to %d bytes; the cap is %d", f.Name, len(expanded), MaxResolvedBytes)
	}
	names := make([]string, 0, len(used))
	for n := range used {
		names = append(names, n)
	}
	sort.Strings(names)
	return expanded, names, nil
}

func (l *Library) expandIncludes(text string, used map[string]bool, depth int, stack map[string]bool) (string, error) {
	if depth > MaxIncludeDepth {
		return "", fmt.Errorf("includes nest deeper than %d", MaxIncludeDepth)
	}
	var expandErr error
	out := includeRef.ReplaceAllStringFunc(text, func(m string) string {
		if expandErr != nil {
			return m
		}
		parts := includeRef.FindStringSubmatch(m)
		name := parts[1]
		if stack[name] {
			expandErr = fmt.Errorf("include cycle through %q", name)
			return m
		}
		inc := l.fragments[name]
		if inc == nil {
			expandErr = fmt.Errorf("include %q not found (fragments/%s.md)", name, name)
			return m
		}
		used[name] = true
		body := inc.Body // includes never pull mode sections; those are the persona's
		for _, kv := range includeArg.FindAllStringSubmatch(parts[2], -1) {
			body = strings.ReplaceAll(body, "{{"+kv[1]+"}}", kv[2])
		}
		stack[name] = true
		nested, err := l.expandIncludes(body, used, depth+1, stack)
		delete(stack, name)
		if err != nil {
			expandErr = err
			return m
		}
		return nested
	})
	return out, expandErr
}

// gitState reads HEAD and dirtiness; a dir that is not a git repo reports no
// commit and dirty (nothing pins the bytes, and the manifest says so).
func gitState(dir string) (commit string, dirty bool) {
	head, err := exec.Command("git", "-C", dir, "rev-parse", "HEAD").Output()
	if err != nil {
		return "", true
	}
	status, err := exec.Command("git", "-C", dir, "status", "--porcelain").Output()
	if err != nil {
		return strings.TrimSpace(string(head)), true
	}
	return strings.TrimSpace(string(head)), len(strings.TrimSpace(string(status))) > 0
}

// Ensure bootstraps the prompts directory: the two subdirectories, a git
// repo, and a starter README the first time. It never touches existing files.
func Ensure(dir string) error {
	fresh := false
	if _, err := os.Stat(dir); os.IsNotExist(err) {
		fresh = true
	}
	for _, sub := range []string{"personas", "fragments"} {
		if err := os.MkdirAll(filepath.Join(dir, sub), 0o755); err != nil {
			return err
		}
	}
	if _, err := os.Stat(filepath.Join(dir, ".git")); os.IsNotExist(err) {
		if out, err := exec.Command("git", "-C", dir, "init", "-q").CombinedOutput(); err != nil {
			return fmt.Errorf("git init %s: %v: %s", dir, err, out)
		}
	}
	if fresh {
		readme := filepath.Join(dir, "README.md")
		if _, err := os.Stat(readme); os.IsNotExist(err) {
			content := `# Forge prompts

Personas and fragments composed into routine prompts at run creation.

- personas/<name>.md — a top-level identity a routine names via its persona
  field. Optional frontmatter: "model: <alias>" (a default the routine can
  override). "## mode: <name>" sections compose only into runs of that mode.
- fragments/<name>.md — building blocks included with {{> name}} or
  {{> name key="value"}} (the value substitutes {{key}} inside that fragment).

No conditionals, no loops: composition is selection. Edits are ordinary git
commits; the daemon reloads the tree and refuses a broken one, keeping the
last good version. "forge persona show <name> --resolved" prints exactly what
an agent will read.
`
			if err := os.WriteFile(readme, []byte(content), 0o644); err != nil {
				return err
			}
		}
	}
	return nil
}
