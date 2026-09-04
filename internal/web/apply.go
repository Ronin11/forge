package web

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/BurntSushi/toml"

	"forge/internal/core/directives"
	"forge/internal/core/model"
	"forge/internal/core/store"
	"forge/internal/tools"
)

// applyProposal applies an approved proposal per its kind (DESIGN.md §12) and
// returns the applied_ref (a generation, a file path, a directory, a branch).
// Store writes happen in the caller's transaction — the approve handler calls
// MarkProposalApplied with the returned ref, so an error here rolls the whole
// approval back and the proposal stays where it was. Filesystem and git
// effects clean up after themselves on failure and refuse to overwrite: a tool
// directory or proposal branch that already exists is an error.
func (s *Engine) applyProposal(ctx context.Context, tx *store.Tx, p *store.Proposal) (string, error) {
	switch p.Kind {
	case model.ProposalRoutine:
		return s.applyRoutine(ctx, tx, p)
	case model.ProposalProcess:
		return s.applyProcess(ctx, tx, p)
	case model.ProposalModePrompt:
		return s.applyModePrompt(p)
	case model.ProposalDoc:
		// Doc content lives in the kb or a repository and is agent-written;
		// the daemon only records that the proposal covered it.
		return "recorded:" + p.Target, nil
	case model.ProposalTool:
		return s.applyTool(ctx, p)
	case model.ProposalCode:
		return s.applyCode(ctx, tx, p)
	}
	return "", fmt.Errorf("proposal %s: kind %q cannot be applied", model.ShortID(p.ID), p.Kind)
}

// routineUpdates are the after-fields a `routine` proposal may set: the
// prompt and settings that define what the routine does.
type routineUpdates struct {
	Prompt         *string   `json:"prompt"`
	Model          *string   `json:"model"`
	Effort         *string   `json:"effort"`
	MaxTurns       *int      `json:"max_turns"`
	TimeoutSeconds *int      `json:"timeout_seconds"`
	MaxBudgetUSD   *float64  `json:"max_budget_usd"`
	AllowedTools   *[]string `json:"allowed_tools"`
}

// applyRoutine creates a new generation carrying only the fields the proposal
// names (source = proposal:<id>); everything else keeps its current value.
func (s *Engine) applyRoutine(ctx context.Context, tx *store.Tx, p *store.Proposal) (string, error) {
	name, err := targetName(p.Target, "routine:")
	if err != nil {
		return "", fmt.Errorf("proposal %s: %w", model.ShortID(p.ID), err)
	}
	var u routineUpdates
	if err := decodeAfter(p.After, &u); err != nil {
		return "", fmt.Errorf("proposal %s (routine): %w", model.ShortID(p.ID), err)
	}
	r, err := tx.GetRoutine(ctx, name)
	if err != nil {
		return "", err
	}
	if u.Model != nil {
		if _, ok := s.resolveModel(*u.Model); !ok {
			return "", fmt.Errorf("proposal %s: unknown model alias %q", model.ShortID(p.ID), *u.Model)
		}
	}
	// A directive-target routine's content lives in the library: prompt,
	// model, and effort updates rewrite directives/<name>.md through the same
	// validate-or-revert path a UI edit takes. Operational fields below still
	// hit the row. Content A/B rides git history — no generation: ref, so the
	// auto-revert sweep deliberately excludes these.
	if kind, dname := targetOf(r); kind == store.TargetDirective {
		var ref string
		if u.Prompt != nil || u.Model != nil || u.Effort != nil {
			if ref, err = s.applyDirectiveContent(ctx, p, dname, directives.DirectiveUpdates{Body: u.Prompt, Model: u.Model, Effort: u.Effort}); err != nil {
				return "", err
			}
		}
		if u.MaxTurns != nil || u.TimeoutSeconds != nil || u.MaxBudgetUSD != nil || u.AllowedTools != nil {
			if u.MaxTurns != nil {
				r.MaxTurns = *u.MaxTurns
			}
			if u.TimeoutSeconds != nil {
				r.TimeoutSeconds = *u.TimeoutSeconds
			}
			if u.MaxBudgetUSD != nil {
				r.MaxBudgetUSD = *u.MaxBudgetUSD
			}
			if u.AllowedTools != nil {
				r.AllowedTools = *u.AllowedTools
			}
			if err := tx.UpdateRoutineFrom(ctx, r, r.Generation, "proposal:"+p.ID); err != nil {
				return "", fmt.Errorf("apply routine proposal %s: %w", model.ShortID(p.ID), err)
			}
			if ref == "" {
				ref = fmt.Sprintf("generation:%d", r.Generation)
			}
		}
		if ref == "" {
			return "", fmt.Errorf("proposal %s: no applicable updates", model.ShortID(p.ID))
		}
		return ref, nil
	}
	if u.Prompt != nil {
		r.Prompt = *u.Prompt
	}
	if u.Model != nil {
		r.Model = *u.Model
	}
	if u.Effort != nil {
		r.Effort = *u.Effort
	}
	if u.MaxTurns != nil {
		r.MaxTurns = *u.MaxTurns
	}
	if u.TimeoutSeconds != nil {
		r.TimeoutSeconds = *u.TimeoutSeconds
	}
	if u.MaxBudgetUSD != nil {
		r.MaxBudgetUSD = *u.MaxBudgetUSD
	}
	if u.AllowedTools != nil {
		r.AllowedTools = *u.AllowedTools
	}
	if err := tx.UpdateRoutineFrom(ctx, r, r.Generation, "proposal:"+p.ID); err != nil {
		return "", fmt.Errorf("apply routine proposal %s: %w", model.ShortID(p.ID), err)
	}
	return fmt.Sprintf("generation:%d", r.Generation), nil
}

// applyDirectiveContent rewrites directives/<name>.md with a content
// proposal's updates: write, validate by reloading the whole tree, revert on
// failure, commit, hot-reload — the putPromptFragment discipline. The file
// commit cannot roll back with the approve transaction; the git history
// keeps it auditable either way.
func (s *Engine) applyDirectiveContent(ctx context.Context, p *store.Proposal, name string, u directives.DirectiveUpdates) (string, error) {
	lib := s.libraryNow()
	if lib == nil {
		return "", fmt.Errorf("proposal %s: this process has no prompts library", model.ShortID(p.ID))
	}
	d := lib.Directive(name)
	if d == nil {
		return "", fmt.Errorf("proposal %s: directive %q is not in the library", model.ShortID(p.ID), name)
	}
	old, err := os.ReadFile(d.Path)
	if err != nil {
		return "", err
	}
	next, err := directives.RewriteDirective(old, u)
	if err != nil {
		return "", fmt.Errorf("proposal %s: %w", model.ShortID(p.ID), err)
	}
	if err := os.WriteFile(d.Path, next, 0o644); err != nil {
		return "", err
	}
	if _, err := directives.Load(lib.Dir); err != nil {
		if rerr := os.WriteFile(d.Path, old, 0o644); rerr != nil {
			s.log.ErrorContext(ctx, "revert refused directive proposal", "path", d.Path, "error", rerr)
		}
		return "", fmt.Errorf("proposal %s: the edit breaks the library: %w", model.ShortID(p.ID), err)
	}
	directives.CommitEdit(lib.Dir, d.Path, "proposal:"+p.ID)
	if s.promptsReload != nil {
		if err := s.promptsReload(); err != nil {
			s.log.WarnContext(ctx, "prompts reload after proposal", "error", err)
		}
	}
	return "directive:" + name, nil
}

// libraryNow is promptLibrary for Engine methods (no HTTP imports here).
func (s *Engine) libraryNow() *directives.Library {
	if s.prompts == nil {
		return nil
	}
	return s.prompts()
}

// processUpdates are the after-fields a `process` proposal may set: when and
// under what constraints the routine runs, not what it does.
type processUpdates struct {
	Schedule        *string   `json:"schedule"`
	ScheduleEnabled *bool     `json:"schedule_enabled"`
	BudgetClass     *string   `json:"budget_class"`
	Autonomy        *string   `json:"autonomy"`
	Priority        *int      `json:"priority"`
	Concurrency     *int      `json:"concurrency"`
	Deps            *[]string `json:"deps"`
}

// applyProcess is applyRoutine for the scheduling fields; it too creates a new
// generation with source = proposal:<id>.
func (s *Engine) applyProcess(ctx context.Context, tx *store.Tx, p *store.Proposal) (string, error) {
	name, err := targetName(p.Target, "routine:")
	if err != nil {
		return "", fmt.Errorf("proposal %s: %w", model.ShortID(p.ID), err)
	}
	var u processUpdates
	if err := decodeAfter(p.After, &u); err != nil {
		return "", fmt.Errorf("proposal %s (process): %w", model.ShortID(p.ID), err)
	}
	r, err := tx.GetRoutine(ctx, name)
	if err != nil {
		return "", err
	}
	if u.Schedule != nil {
		r.Schedule = *u.Schedule
	}
	if u.ScheduleEnabled != nil {
		r.ScheduleEnabled = *u.ScheduleEnabled
	}
	if u.BudgetClass != nil {
		bc := model.BudgetClass(*u.BudgetClass)
		if !bc.Valid() {
			return "", fmt.Errorf("proposal %s: budget_class %q", model.ShortID(p.ID), *u.BudgetClass)
		}
		r.BudgetClass = bc
	}
	if u.Autonomy != nil {
		a := model.Autonomy(*u.Autonomy)
		if a != "" && !a.Valid() {
			return "", fmt.Errorf("proposal %s: autonomy %q", model.ShortID(p.ID), *u.Autonomy)
		}
		r.Autonomy = a
	}
	if u.Priority != nil {
		r.Priority = *u.Priority
	}
	if u.Concurrency != nil {
		r.Concurrency = *u.Concurrency
	}
	if u.Deps != nil {
		r.Deps = *u.Deps
	}
	if err := tx.UpdateRoutineFrom(ctx, r, r.Generation, "proposal:"+p.ID); err != nil {
		return "", fmt.Errorf("apply process proposal %s: %w", model.ShortID(p.ID), err)
	}
	return fmt.Sprintf("generation:%d", r.Generation), nil
}

// applyModePrompt writes the live mode preamble <home>/modes/<name>.md, which
// prompt assembly already prefers over the embedded default (prompt.go). An
// existing file is first copied to <name>.md.prev-<id8> so the previous
// preamble survives. No context: a few local file operations only.
func (s *Engine) applyModePrompt(p *store.Proposal) (string, error) {
	name, err := targetName(p.Target, "mode:")
	if err != nil {
		return "", fmt.Errorf("proposal %s: %w", model.ShortID(p.ID), err)
	}
	var u struct {
		Content *string `json:"content"`
	}
	if err := decodeAfter(p.After, &u); err != nil {
		return "", fmt.Errorf("proposal %s (mode_prompt): %w", model.ShortID(p.ID), err)
	}
	if u.Content == nil || *u.Content == "" {
		return "", fmt.Errorf("proposal %s: after.content is required", model.ShortID(p.ID))
	}
	dir := filepath.Join(s.home, "modes")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", fmt.Errorf("create %s: %w", dir, err)
	}
	path := filepath.Join(dir, name+".md")
	if prev, err := os.ReadFile(path); err == nil {
		backup := filepath.Join(dir, name+".md.prev-"+model.ShortID(p.ID))
		if err := os.WriteFile(backup, prev, 0o600); err != nil {
			return "", fmt.Errorf("back up %s: %w", path, err)
		}
	} else if !os.IsNotExist(err) {
		return "", fmt.Errorf("read %s: %w", path, err)
	}
	if err := os.WriteFile(path, []byte(*u.Content), 0o600); err != nil {
		return "", fmt.Errorf("write %s: %w", path, err)
	}
	return path, nil
}

// toolAfter is a `tool` proposal's after: the manifest plus the script files
// that make up <home>/tools/<name>/.
type toolAfter struct {
	Manifest struct {
		Name           string          `json:"name"`
		Description    string          `json:"description"`
		InputSchema    json.RawMessage `json:"input_schema"`
		Command        []string        `json:"command"`
		TimeoutSeconds int             `json:"timeout_seconds"`
		TestCommand    []string        `json:"test_command"`
	} `json:"manifest"`
	Files map[string]string `json:"files"`
}

// applyTool writes <home>/tools/<name>/ (manifest.toml + files) and runs the
// tool's test there — the "its tests pass" gate. A test failure removes the
// directory and returns the error, so the approve transaction rolls back and
// the proposal stays proposed.
func (s *Engine) applyTool(ctx context.Context, p *store.Proposal) (string, error) {
	name := p.Target
	if err := model.ValidateName(name); err != nil {
		return "", fmt.Errorf("proposal %s: %w", model.ShortID(p.ID), err)
	}
	var u toolAfter
	if err := decodeAfter(p.After, &u); err != nil {
		return "", fmt.Errorf("proposal %s (tool): %w", model.ShortID(p.ID), err)
	}
	m := u.Manifest
	if m.Name != name {
		return "", fmt.Errorf("proposal %s: manifest name %q != target %q", model.ShortID(p.ID), m.Name, name)
	}
	if len(m.Command) == 0 {
		return "", fmt.Errorf("proposal %s: manifest command is required", model.ShortID(p.ID))
	}
	var schema map[string]json.RawMessage
	if err := json.Unmarshal(m.InputSchema, &schema); err != nil {
		return "", fmt.Errorf("proposal %s: input_schema is not a JSON object: %w", model.ShortID(p.ID), err)
	}
	dir := filepath.Join(s.home, "tools", name)
	if _, err := os.Stat(dir); err == nil {
		return "", fmt.Errorf("tool %s already exists at %s", name, dir)
	} else if !os.IsNotExist(err) {
		return "", fmt.Errorf("stat %s: %w", dir, err)
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", fmt.Errorf("create %s: %w", dir, err)
	}
	// The store transaction rolls back on error; the directory must roll back
	// with it so a rejected tool leaves nothing behind.
	applied := false
	defer func() {
		if !applied {
			if rerr := os.RemoveAll(dir); rerr != nil {
				s.log.WarnContext(ctx, "remove failed tool dir", "dir", dir, "error", rerr)
			}
		}
	}()
	for rel, content := range u.Files {
		clean := filepath.Clean(rel)
		if clean == "." || filepath.IsAbs(clean) || clean == ".." || strings.HasPrefix(clean, "../") {
			return "", fmt.Errorf("proposal %s: file path %q escapes the tool directory", model.ShortID(p.ID), rel)
		}
		path := filepath.Join(dir, clean)
		if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
			return "", fmt.Errorf("create %s: %w", filepath.Dir(path), err)
		}
		perm := os.FileMode(0o644)
		if strings.HasSuffix(clean, ".sh") {
			perm = 0o755
		}
		if err := os.WriteFile(path, []byte(content), perm); err != nil {
			return "", fmt.Errorf("write %s: %w", path, err)
		}
	}
	manifest := tools.ScriptManifest{Name: m.Name, Description: m.Description, InputSchema: string(m.InputSchema),
		Command: m.Command, TimeoutSeconds: m.TimeoutSeconds, TestCommand: m.TestCommand}
	var buf bytes.Buffer
	if err := toml.NewEncoder(&buf).Encode(manifest); err != nil {
		return "", fmt.Errorf("encode manifest for %s: %w", name, err)
	}
	if err := os.WriteFile(filepath.Join(dir, "manifest.toml"), buf.Bytes(), 0o644); err != nil {
		return "", fmt.Errorf("write manifest for %s: %w", name, err)
	}
	if len(m.TestCommand) > 0 {
		if err := tools.ScriptTest(ctx, dir, m.TestCommand); err != nil {
			return "", fmt.Errorf("tool %s: %w", name, err)
		}
	}
	applied = true
	return dir, nil
}

// applyCode turns a code proposal into a forge/proposal-<id8> branch on the
// registered "forge" repository, holding PROPOSAL.md and (when present) the
// diff as a file — never applied, never merged, never pushed (DESIGN.md §12).
// A temporary worktree under <home>/scratch keeps the registered checkout's
// working tree, index, and HEAD untouched.
func (s *Engine) applyCode(ctx context.Context, tx *store.Tx, p *store.Proposal) (string, error) {
	repos, err := tx.Repositories(ctx)
	if err != nil {
		return "", fmt.Errorf("list repositories: %w", err)
	}
	var repoPath string
	for _, r := range repos {
		if r.Name == "forge" {
			repoPath = r.Path
			break
		}
	}
	if repoPath == "" {
		return "", fmt.Errorf("code proposal %s: no repository named %q is registered", model.ShortID(p.ID), "forge")
	}
	id8 := model.ShortID(p.ID)
	branch := "forge/proposal-" + id8
	if _, err := gitOutput(ctx, repoPath, nil, "rev-parse", "--verify", "--quiet", "refs/heads/"+branch); err == nil {
		return "", fmt.Errorf("code proposal %s: branch %s already exists", id8, branch)
	}
	scratch := filepath.Join(s.home, "scratch")
	if err := os.MkdirAll(scratch, 0o700); err != nil {
		return "", fmt.Errorf("create %s: %w", scratch, err)
	}
	worktree, err := os.MkdirTemp(scratch, "proposal-"+id8+"-")
	if err != nil {
		return "", fmt.Errorf("scratch worktree: %w", err)
	}
	if _, err := gitOutput(ctx, repoPath, nil, "worktree", "add", "-b", branch, worktree, "HEAD"); err != nil {
		if rerr := os.RemoveAll(worktree); rerr != nil {
			s.log.WarnContext(ctx, "remove scratch dir", "dir", worktree, "error", rerr)
		}
		return "", fmt.Errorf("code proposal %s: %w", id8, err)
	}
	// fail undoes the half-made branch and worktree; the original error wins.
	fail := func(step error) (string, error) {
		if _, err := gitOutput(ctx, repoPath, nil, "worktree", "remove", "--force", worktree); err != nil {
			s.log.WarnContext(ctx, "remove proposal worktree", "worktree", worktree, "error", err)
		}
		if _, err := gitOutput(ctx, repoPath, nil, "branch", "-D", branch); err != nil {
			s.log.WarnContext(ctx, "delete proposal branch", "branch", branch, "error", err)
		}
		return "", fmt.Errorf("code proposal %s: %w", id8, step)
	}
	if err := os.WriteFile(filepath.Join(worktree, "PROPOSAL.md"), []byte(proposalDoc(p)), 0o644); err != nil {
		return fail(fmt.Errorf("write PROPOSAL.md: %w", err))
	}
	diff, hasDiff, err := afterDiff(p.After)
	if err != nil {
		return fail(err)
	}
	if hasDiff {
		// The diff is committed as a file for a human to read — never applied
		// to the source: code proposals stop at the branch.
		if err := os.WriteFile(filepath.Join(worktree, "proposal.diff"), []byte(diff), 0o644); err != nil {
			return fail(fmt.Errorf("write proposal.diff: %w", err))
		}
	}
	if _, err := gitOutput(ctx, worktree, nil, "add", "-A"); err != nil {
		return fail(err)
	}
	// Authorship travels by env only — never any .git/config (STYLE.md §10).
	authorEnv := []string{"GIT_AUTHOR_NAME=forge", "GIT_AUTHOR_EMAIL=forge@local",
		"GIT_COMMITTER_NAME=forge", "GIT_COMMITTER_EMAIL=forge@local"}
	if _, err := gitOutput(ctx, worktree, authorEnv, "commit", "-m", "proposal "+id8+": "+p.Target); err != nil {
		return fail(err)
	}
	if _, err := gitOutput(ctx, repoPath, nil, "worktree", "remove", worktree); err != nil {
		return fail(err)
	}
	return branch, nil
}

// proposalDoc renders PROPOSAL.md: the human-readable record the branch carries.
func proposalDoc(p *store.Proposal) string {
	var b strings.Builder
	fmt.Fprintf(&b, "# Proposal %s\n\n", model.ShortID(p.ID))
	fmt.Fprintf(&b, "- Kind: %s\n- Target: %s\n- Source: %s\n\n", p.Kind, p.Target, p.Source)
	fmt.Fprintf(&b, "## Rationale\n\n%s\n\n## Verification plan\n\n%s\n", p.Rationale, p.VerificationPlan)
	if doc := prettyJSON(p.Before); doc != "" {
		fmt.Fprintf(&b, "\n## Before\n\n```json\n%s\n```\n", doc)
	}
	if doc := prettyJSON(p.After); doc != "" {
		fmt.Fprintf(&b, "\n## After\n\n```json\n%s\n```\n", doc)
	}
	return b.String()
}

// prettyJSON indents a raw document for PROPOSAL.md; invalid JSON is kept
// verbatim rather than lost.
func prettyJSON(raw json.RawMessage) string {
	if len(raw) == 0 {
		return ""
	}
	var buf bytes.Buffer
	if err := json.Indent(&buf, raw, "", "  "); err != nil {
		return string(raw)
	}
	return buf.String()
}

// afterDiff extracts the optional "diff" string from a code proposal's after.
func afterDiff(raw json.RawMessage) (diff string, ok bool, err error) {
	if len(raw) == 0 {
		return "", false, nil
	}
	var m map[string]json.RawMessage
	if err := json.Unmarshal(raw, &m); err != nil {
		return "", false, fmt.Errorf("after: %w", err)
	}
	d, present := m["diff"]
	if !present {
		return "", false, nil
	}
	if err := json.Unmarshal(d, &diff); err != nil {
		return "", false, fmt.Errorf("after.diff: want a string: %w", err)
	}
	return diff, true, nil
}

// decodeAfter reads a proposal's after strictly: a field the kind does not
// allow is an error, because the human approved exactly these updates.
func decodeAfter(raw json.RawMessage, v any) error {
	if len(raw) == 0 {
		return fmt.Errorf("after is required")
	}
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.DisallowUnknownFields()
	if err := dec.Decode(v); err != nil {
		return fmt.Errorf("after: %w", err)
	}
	return nil
}

// targetName strips the kind's prefix from a proposal target and validates the
// remainder as a Forge name.
func targetName(target, prefix string) (string, error) {
	// The prefix is optional: retro agents file bare names ("inventory") as
	// often as fact-link grammar ("routine:inventory"); both are unambiguous
	// here because the kind picks the prefix.
	name, ok := strings.CutPrefix(target, prefix)
	if !ok {
		name = target
	}

	if err := model.ValidateName(name); err != nil {
		return "", err
	}
	return name, nil
}

// gitOutput runs one git command against dir with bounded, combined output in
// the error. extraEnv is appended to the process environment — configuration
// and authorship travel by env only, never any .git/config.
func gitOutput(ctx context.Context, dir string, extraEnv []string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, "git", append([]string{"-C", dir}, args...)...)
	cmd.Env = append(os.Environ(), extraEnv...)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("git %s: %v: %s", strings.Join(args, " "), err, gitTail(out))
	}
	return string(out), nil
}

// gitTail bounds git output quoted in errors.
func gitTail(out []byte) string {
	const n = 2048
	s := strings.TrimSpace(string(out))
	if len(s) > n {
		s = "…" + s[len(s)-n:]
	}
	return s
}
