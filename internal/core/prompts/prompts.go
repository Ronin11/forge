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

// Fragment returns any fragment (personas included) by name, or nil.
func (l *Library) Fragment(name string) *Fragment { return l.fragments[name] }

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
		if err := os.WriteFile(filepath.Join(dir, "README.md"), []byte(readmeContent), 0o644); err != nil {
			return err
		}
		err := fs.WalkDir(starterFS, "starter", func(path string, d fs.DirEntry, err error) error {
			if err != nil || d.IsDir() {
				return err
			}
			rel, err := filepath.Rel("starter", path)
			if err != nil {
				return err
			}
			content, err := starterFS.ReadFile(path)
			if err != nil {
				return err
			}
			dst := filepath.Join(dir, rel)
			if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
				return err
			}
			return os.WriteFile(dst, content, 0o644)
		})
		if err != nil {
			return err
		}
		bootstrapCommit(dir)
	}
	return nil
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
const readmeContent = `# Forge prompts

Personas and fragments, composed into routine prompts when a run is created.
Edits are ordinary git commits: the daemon reloads this tree every 30s,
refuses a broken one (unknown includes, cycles, nesting deeper than 8,
resolutions past 256 KiB), and keeps the last good version until you fix it.

    forge persona list
    forge persona show <name> --resolved [--mode <mode>]   # the exact bytes an agent reads

## Layout

- personas/<name>.md — a top-level identity. Routines name it in their
  persona field; "task add --persona <name>" and a workflow routine-node's
  config {"persona": ...} use it per run.
- fragments/<name>.md — building blocks; subdirectories are fine
  (fragments/house-style/go.md includes as {{> house-style/go}}).
  Names are lower-case slugs.

## Frontmatter (optional, personas)

    ---
    model: opus        # default model alias; the routine's own model wins
    ---

## Composition

There are exactly three constructs — no conditionals, no loops. Teaching is
selection, not branching: an agent (or you) must be able to read any file top
to bottom and know what it says.

- {{> name}} — include a fragment's body.
- {{> name key="value"}} — include with parameters: every {{key}} inside
  *that fragment only* becomes value. Parameters you do not pass stay as-is.
- ## mode: <name> — a persona section composed only into runs of that mode
  (the routine's mode). Everything above the first mode heading is the core,
  always included; a mode with no section just gets the core. Includes work
  inside mode sections; included fragments never contribute their own mode
  sections.

## Variables

Composition happens first; later stages substitute into the composed text.
Everything the library does not own passes through untouched.

| Variable | Where it works | Replaced by | When |
|---|---|---|---|
| {{> name}}, {{> name k="v"}} | persona and fragment bodies | the fragment's body | composition (run creation) |
| {{k}} | inside a fragment given k="v" | the parameter value | composition |
| {{objective}} | anywhere in the final prompt (persona text included) | the run's objective, or a self-directed fallback | work creation |
| {{repo}} | anywhere in the final prompt | the repository this attempt targets | claim time (per repository) |
| {{run.objective}} | workflow routine-node *objectives* only | the workflow run's objective | node materialization |
| {{run.repositories}} | workflow routine-node objectives only | the run's repositories, comma-joined | node materialization |
| {{run.workflow}} | workflow routine-node objectives only | the workflow name | node materialization |
| {{run.id}} | workflow routine-node objectives only | the run id | node materialization |
| {{steps.<node>.status}} | workflow routine-node objectives only | the upstream node's status | node materialization |
| {{steps.<node>.output.<dot.path>}} | workflow routine-node objectives only | a value from the upstream node's output | node materialization |

The {{run.*}} and {{steps.*}} forms belong to the workflow engine, not this
library: they expand in a routine node's objective, which then replaces
{{objective}} wherever the composed prompt says it. Putting them directly in
a persona or fragment does nothing — they pass through unresolved.

## What runs where

The final prompt an agent reads is, in order:

    resolved persona (core + the routine's mode section)
    routine prompt (the task text)

with {{objective}} and {{repo}} substituted as above. Every run freezes the
resolved bytes plus a composition manifest (each fragment's content hash and
this repo's commit) into its snapshot — "what did this run read" never
depends on the working tree, and a dirty tree is recorded as dirty.
`
