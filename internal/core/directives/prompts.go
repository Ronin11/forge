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
package directives

import (
	"crypto/sha256"
	"embed"
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

// starterFS is the shipped library: standard software roles built from shared
// fragments, written once on a fresh bootstrap and then owned by the user's
// git history like anything else in the tree.
//
//go:embed starter
var starterFS embed.FS

// MaxIncludeDepth bounds nesting; a tree deeper than this is a maze, not a
// library.
const MaxIncludeDepth = 8

// MaxResolvedBytes caps one resolved persona; past this the prompt is doing a
// document's job.
const MaxResolvedBytes = 256 * 1024

// Fragment is one file: a fragment proper, a persona (under personas/, may
// declare a model and mode sections), or a directive (under directives/ —
// executable task content: frontmatter names its mode, and optionally a
// persona, model, and effort; the body is the task text a routine trigger or
// workflow node runs).
type Fragment struct {
	Name    string `json:"name"` // relative path minus .md: "escalation-rules", "house-style/go"
	Path    string `json:"path"`
	Model   string `json:"model,omitempty"` // personas/directives: default model alias; overrides win
	Body    string `json:"-"`               // core body, mode sections split out
	Modes   map[string]string
	Hash    string `json:"hash"` // sha256 of the raw file
	Persona bool   `json:"persona"`
	// Description is optional metadata for search and tool exposure (any
	// kind: frontmatter `description:`; scripts: the header block).
	Description string `json:"description,omitempty"`
	// Directive marks executable task content; the remaining fields are
	// directive frontmatter only.
	Directive bool `json:"directive,omitempty"`
	// Script marks scripts/<name>.<ext> — any language, with a parsed
	// comment header (scripts.go). Body is the full raw source. Interpreter
	// is the resolved argv prefix for a subprocess script; empty means .js,
	// run in-process in the goja sandbox.
	Script      bool     `json:"script,omitempty"`
	Interpreter []string `json:"interpreter,omitempty"`
	InputSchema string   `json:"input_schema,omitempty"` // scripts: JSON schema for tool input
	TimeoutMS   int      `json:"timeout_ms,omitempty"`   // scripts: execution cap
	// Tool marks content agents may invoke through the bridge tools
	// (scripts + directives).
	Tool       bool   `json:"tool,omitempty"`
	PersonaRef string `json:"persona_ref,omitempty"` // persona composed ahead of the body
	Mode       string `json:"mode,omitempty"`        // required for directives
	Effort     string `json:"effort,omitempty"`
}

// fragmentKind selects the parse rules for one library subdir.
type fragmentKind int

const (
	kindFragment fragmentKind = iota
	kindPersona
	kindDirective
	kindScript
)

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
	kinds := map[string]fragmentKind{"personas": kindPersona, "fragments": kindFragment, "directives": kindDirective, "scripts": kindScript}
	for _, sub := range []string{"personas", "fragments", "directives", "scripts"} {
		root := filepath.Join(dir, sub)
		err := filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
			if err != nil {
				if os.IsNotExist(err) && path == root {
					return filepath.SkipDir
				}
				return err
			}
			if d.IsDir() {
				return nil
			}
			ext := filepath.Ext(path)
			if kinds[sub] == kindScript {
				// Scripts may be any language: .js runs in-process (goja);
				// everything else runs as a subprocess. Dotfiles skip.
				if ext == "" || strings.HasPrefix(d.Name(), ".") {
					return nil
				}
			} else if ext != ".md" {
				return nil
			}
			rel, err := filepath.Rel(root, path)
			if err != nil {
				return err
			}
			f, err := loadFragment(path, filepath.ToSlash(strings.TrimSuffix(rel, ext)), kinds[sub])
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
		if f.Script {
			continue // compile-checked at parse; no includes to expand
		}
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

func loadFragment(path, name string, kind fragmentKind) (*Fragment, error) {
	if err := ValidateName(name); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	if kind == kindScript {
		return parseScript(raw, path, name)
	}
	return parseFragment(raw, path, name, kind)
}

// parseFragment builds a fragment from raw bytes — the file loader and
// in-memory variants (WithVariant) share it.
func parseFragment(raw []byte, path, name string, kind fragmentKind) (*Fragment, error) {
	sum := sha256.Sum256(raw)
	f := &Fragment{Name: name, Path: path, Hash: hex.EncodeToString(sum[:]),
		Persona: kind == kindPersona, Directive: kind == kindDirective, Modes: map[string]string{}}
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
			key, val = strings.TrimSpace(key), strings.TrimSpace(val)
			switch {
			case key == "description":
				f.Description = val
			case key == "tool" && kind == kindDirective:
				switch val {
				case "true":
					f.Tool = true
				case "false":
				default:
					return nil, fmt.Errorf("%s: frontmatter tool %q: want true or false", path, val)
				}
			case key == "model":
				f.Model = val
			case key == "mode" && kind == kindDirective:
				f.Mode = val
			case key == "persona" && kind == kindDirective:
				if err := ValidateName(val); err != nil {
					return nil, fmt.Errorf("%s: persona: %w", path, err)
				}
				f.PersonaRef = val
			case key == "effort" && kind == kindDirective:
				f.Effort = val
			default:
				return nil, fmt.Errorf("%s: unknown frontmatter key %q", path, key)
			}
		}
		body = strings.TrimPrefix(rest[end+4:], "\n")
	}
	if kind == kindDirective {
		if f.Mode == "" {
			return nil, fmt.Errorf("%s: a directive needs `mode:` in its frontmatter", path)
		}
		// Directives have exactly one mode (frontmatter); a `## mode:` heading
		// in the body is just markdown, not a section.
		f.Body = strings.TrimSpace(body)
		return f, nil
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

// Fragment returns any fragment (personas included) by name, or nil.
func (l *Library) Fragment(name string) *Fragment { return l.fragments[name] }

// WithVariant derives a library with one fragment's content replaced in
// memory — the optimization loop's way to compose candidate edits without
// touching disk. The variant is validated the way Load validates: every
// persona (and the variant itself) must still expand cleanly.
func (l *Library) WithVariant(name, raw string) (*Library, error) {
	orig := l.fragments[name]
	if orig == nil {
		return nil, fmt.Errorf("fragment %q is not in the library", name)
	}
	kind := kindFragment
	switch {
	case orig.Persona:
		kind = kindPersona
	case orig.Directive:
		kind = kindDirective
	case orig.Script:
		kind = kindScript
	}
	var f *Fragment
	var err error
	if kind == kindScript {
		f, err = parseScript([]byte(raw), orig.Path, name)
	} else {
		f, err = parseFragment([]byte(raw), orig.Path, name, kind)
	}
	if err != nil {
		return nil, err
	}
	next := &Library{Dir: l.Dir, Commit: l.Commit, Dirty: true, LoadedAt: l.LoadedAt, fragments: make(map[string]*Fragment, len(l.fragments))}
	for k, v := range l.fragments {
		next.fragments[k] = v
	}
	next.fragments[name] = f
	for _, frag := range next.fragments {
		if frag.Script || (!frag.Persona && !frag.Directive && frag.Name != name) {
			continue
		}
		if _, _, err := next.expand(frag, "", nil, 0); err != nil {
			return nil, err
		}
		for mode := range frag.Modes {
			if _, _, err := next.expand(frag, mode, nil, 0); err != nil {
				return nil, fmt.Errorf("mode %s: %w", mode, err)
			}
		}
	}
	return next, nil
}

// Persona returns a persona by name, or nil.
func (l *Library) Persona(name string) *Fragment {
	f := l.fragments[name]
	if f == nil || !f.Persona {
		return nil
	}
	return f
}

// Directive returns a directive by name, or nil.
func (l *Library) Directive(name string) *Fragment {
	f := l.fragments[name]
	if f == nil || !f.Directive {
		return nil
	}
	return f
}

// Script returns a library script by name, or nil.
func (l *Library) Script(name string) *Fragment {
	f := l.fragments[name]
	if f == nil || !f.Script {
		return nil
	}
	return f
}

// ResolveDirectiveBody expands a directive's task text — includes resolved,
// {{objective}}/{{repo}}/{{run.*}} left for the downstream substitutions —
// and returns the manifest of every fragment that went in. The directive's
// persona is NOT composed here: persona composition stays with the caller
// (the same seam routines use), checked at resolve time so one bad reference
// cannot brick the whole library.
func (l *Library) ResolveDirectiveBody(name string) (string, []ManifestEntry, error) {
	f := l.Directive(name)
	if f == nil {
		return "", nil, fmt.Errorf("directive %q is not in the library (directives/%s.md)", name, name)
	}
	used := map[string]bool{}
	text, names, err := l.expand(f, "", used, 0)
	if err != nil {
		return "", nil, err
	}
	entries := make([]ManifestEntry, 0, len(names))
	for _, n := range names {
		entries = append(entries, ManifestEntry{Name: n, Hash: l.fragments[n].Hash})
	}
	return text, entries, nil
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

// DirectiveUpdates are the fields RewriteDirective may replace; nil keeps the
// current value.
type DirectiveUpdates struct {
	Body   *string
	Model  *string
	Effort *string
}

// RewriteDirective returns a directive file's raw bytes with the given fields
// replaced: the body swapped wholesale, model/effort updated (or added) in
// the frontmatter, everything else — mode, persona, key order, comments —
// preserved. The proposal-apply path uses it so a content proposal edits the
// file the way a human would.
func RewriteDirective(raw []byte, u DirectiveUpdates) ([]byte, error) {
	text := string(raw)
	if !strings.HasPrefix(text, "---\n") {
		return nil, fmt.Errorf("not a directive file: missing frontmatter")
	}
	rest := text[4:]
	end := strings.Index(rest, "\n---")
	if end < 0 {
		return nil, fmt.Errorf("unterminated frontmatter")
	}
	lines := strings.Split(rest[:end], "\n")
	set := func(key string, val *string) {
		if val == nil {
			return
		}
		for i, line := range lines {
			k, _, ok := strings.Cut(strings.TrimSpace(line), ":")
			if ok && strings.TrimSpace(k) == key {
				if *val == "" {
					lines = append(lines[:i], lines[i+1:]...)
				} else {
					lines[i] = key + ": " + *val
				}
				return
			}
		}
		if *val != "" {
			lines = append(lines, key+": "+*val)
		}
	}
	set("model", u.Model)
	set("effort", u.Effort)
	body := strings.TrimPrefix(rest[end+4:], "\n")
	if u.Body != nil {
		body = strings.TrimSpace(*u.Body) + "\n"
	}
	return []byte("---\n" + strings.Join(lines, "\n") + "\n---\n" + body), nil
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

// Ensure bootstraps the library directory (subdirectories, a git repo, the
// README the first time) and additively syncs the embedded base: any starter
// file the tree lacks is copied in, and a file that exists — however
// modified — is never touched. A binary upgrade thus delivers new base
// content to existing installs while the user's fork stays theirs; a base
// file deleted locally does return on the next boot (remove it upstream, or
// live with the git history making the re-add visible). Anything copied is
// committed best-effort.
func Ensure(dir string) error {
	fresh := false
	if _, err := os.Stat(dir); os.IsNotExist(err) {
		fresh = true
	}
	for _, sub := range []string{"personas", "fragments", "directives", "scripts"} {
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
		if err := os.WriteFile(filepath.Join(dir, "README.md"), []byte(readmeContent), 0o644); err != nil {
			return err
		}
	}
	copied := 0
	err := fs.WalkDir(starterFS, "starter", func(path string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return err
		}
		rel, err := filepath.Rel("starter", path)
		if err != nil {
			return err
		}
		dst := filepath.Join(dir, rel)
		if _, err := os.Stat(dst); err == nil {
			return nil // the user's copy wins, always
		}
		content, err := starterFS.ReadFile(path)
		if err != nil {
			return err
		}
		if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
			return err
		}
		copied++
		return os.WriteFile(dst, content, 0o644)
	})
	if err != nil {
		return err
	}
	if fresh {
		bootstrapCommit(dir)
	} else if copied > 0 {
		CommitEdit(dir, ".", "forge: base library update")
	}
	ensureUpstream(dir)
	return nil
}

// ensureUpstream points the library's `upstream` remote at the base repo the
// binary shipped with (the starter's UPSTREAM file), so `forge directives
// update` can pull base improvements. Best-effort; an existing remote is
// never changed — the user's fork is theirs.
func ensureUpstream(dir string) {
	raw, err := os.ReadFile(filepath.Join(dir, "UPSTREAM"))
	if err != nil {
		return
	}
	url := strings.TrimSpace(string(raw))
	if url == "" {
		return
	}
	if err := exec.Command("git", "-C", dir, "remote", "get-url", "upstream").Run(); err == nil {
		return
	}
	if err := exec.Command("git", "-C", dir, "remote", "add", "upstream", url).Run(); err != nil {
		return
	}
}

// CommitEdit commits one edited file — the UI's save path. Best-effort like
// the bootstrap commit: where git balks (no identity, hooks) the tree just
// stays dirty, which the next manifest records honestly.
func CommitEdit(dir, path, message string) {
	if err := exec.Command("git", "-C", dir, "add", path).Run(); err != nil {
		return
	}
	if err := exec.Command("git", "-C", dir, "-c", "user.name=forge", "-c", "user.email=forge@localhost", "commit", "-q", "-m", message, "--", path).Run(); err != nil {
		return
	}
}

// bootstrapCommit commits the starter library so the tree starts clean and
// the first run's manifest pins to a commit. Best-effort by design: a host
// where git balks leaves the library dirty until the user commits, which the
// manifest records honestly.
func bootstrapCommit(dir string) {
	if err := exec.Command("git", "-C", dir, "add", "-A").Run(); err != nil {
		return
	}
	if err := exec.Command("git", "-C", dir, "-c", "user.name=forge", "-c", "user.email=forge@localhost", "commit", "-q", "-m", "forge: starter prompt library").Run(); err != nil {
		return
	}
}

// readmeContent is written once at bootstrap: the reference for everything a
// prompt author can use, kept next to the files it documents.
const readmeContent = `# Forge directives

The library of everything an agent reads: directives (executable task
content), personas (identities), and fragments (shared building blocks).
Edits are ordinary git commits: the daemon reloads this tree every 30s,
refuses a broken one (unknown includes, cycles, nesting deeper than 8,
resolutions past 256 KiB), and keeps the last good version until you fix it.

    forge persona list
    forge persona show <name> --resolved [--mode <mode>]   # the exact bytes an agent reads

## Layout

- directives/<name>.md — executable task content. A trigger routine's
  target ("directive:<name>") runs it on a schedule or by hand; a workflow
  directive-node runs it as one graph step. Frontmatter names its mode
  (required) and optionally a persona, model, and effort; the body is the
  task text.
- personas/<name>.md — a top-level identity, composed ahead of the task
  text. Directives name one in frontmatter; "task add --persona <name>" and
  a workflow node's config {"persona": ...} override it per run.
- fragments/<name>.md — building blocks; subdirectories are fine
  (fragments/house-style/go.md includes as {{> house-style/go}}).
  Names are lower-case slugs.
- scripts/<name>.<ext> — any language. .js runs in-process (goja: no
  filesystem or network, the safe default); anything else runs as a daemon
  subprocess — shebang or extension-mapped interpreter (.py/.sh/.rb/.pl),
  JSON input on stdin, one JSON value on stdout. Referenced by workflow
  script nodes ({"script": "<name>"}), schedulable via a routine target
  "script:<name>", and — with tool: true — callable by agents through
  forge_script_run.

## Frontmatter

Directives (mode: required):

    ---
    mode: run          # the execution mode (run, plan, review, ...)
    persona: triager   # optional; composed ahead of the body
    model: opus        # optional; the persona default fills in when absent
    effort: low        # optional model effort knob
    ---

Personas (all optional):

    ---
    model: opus        # default model alias; the directive's own model wins
    ---

Every .md kind may also carry description: (one line, used by search and the
library tool). Directives may carry tool: true to become agent-callable via
forge_directive_run.

Scripts carry their metadata in a leading comment block with the same keys
in every language — the file runs exactly as authored:

    /**forge                        #!/usr/bin/env python3
     * description: what it does    #forge
     * input: {"type":"object"}     # description: what it does
     * timeout_ms: 10000            # input: {"type":"object"}
     * tool: true                   # tool: true
     */

tool: true requires description and input. Unknown keys are load errors.
Subprocess scripts may declare timeout_ms up to 300000; .js caps at 30000.

## Composition

There are exactly three constructs — no conditionals, no loops. Teaching is
selection, not branching: an agent (or you) must be able to read any file top
to bottom and know what it says.

- {{> name}} — include a fragment's body.
- {{> name key="value"}} — include with parameters: every {{key}} inside
  *that fragment only* becomes value. Parameters you do not pass stay as-is.
- ## mode: <name> — a persona section composed only into runs of that mode
  (the directive's mode). Everything above the first mode heading is the
  core, always included; a mode with no section just gets the core. Includes
  work inside mode sections; included fragments never contribute their own
  mode sections. In a directive body a "## mode:" heading is plain markdown —
  a directive has exactly one mode, in its frontmatter.

## Variables

Composition happens first; later stages substitute into the composed text.
Everything the library does not own passes through untouched.

| Variable | Where it works | Replaced by | When |
|---|---|---|---|
| {{> name}}, {{> name k="v"}} | directive, persona, and fragment bodies | the fragment's body | composition (run creation) |
| {{k}} | inside a fragment given k="v" | the parameter value | composition |
| {{objective}} | anywhere in the final prompt (persona text included) | the run's objective, or a self-directed fallback | work creation |
| {{repo}} | anywhere in the final prompt | the repository this attempt targets | claim time (per repository) |
| {{run.objective}} | workflow node *objectives* only | the workflow run's objective | node materialization |
| {{run.repositories}} | workflow node objectives only | the run's repositories, comma-joined | node materialization |
| {{run.workflow}} | workflow node objectives only | the workflow name | node materialization |
| {{run.id}} | workflow node objectives only | the run id | node materialization |
| {{steps.<node>.status}} | workflow node objectives only | the upstream node's status | node materialization |
| {{steps.<node>.state}} | workflow node objectives only | the upstream Work's final state | node materialization |
| {{steps.<node>.summary}} | workflow node objectives only | the upstream attempt's result summary | node materialization |
| {{steps.<node>.output.<dot.path>}} | workflow node objectives only | a value from the upstream node's output | node materialization |

The {{run.*}} and {{steps.*}} forms belong to the workflow engine, not this
library: they expand in a directive node's objective, which then replaces
{{objective}} wherever the composed prompt says it. Putting them directly in
a directive, persona, or fragment does nothing — they pass through unresolved.

## What runs where

The final prompt an agent reads is, in order:

    resolved persona (core + the directive's mode section)
    directive body (the task text, includes expanded)

with {{objective}} and {{repo}} substituted as above. Every run freezes the
resolved bytes plus a composition manifest (each fragment's content hash and
this repo's commit) into its snapshot — "what did this run read" never
depends on the working tree, and a dirty tree is recorded as dirty.
`
