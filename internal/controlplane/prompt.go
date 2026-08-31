package controlplane

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"unicode/utf8"

	"forge/internal/kb"
	"forge/internal/model"
	"forge/internal/store"
)

// promptInput is everything assembly needs (MODES.md "Prompt assembly").
type promptInput struct {
	Mode          string
	RoutinePrompt string
	Repository    string
	RepoPath      string // the registered checkout, for .forge/modes overlays
	Autonomy      model.Autonomy
	Checkpoints   []string
	Home          string // <forge home>, for the live mode preamble files
	// ModePreamble is the embedded default (from the registry); the live file
	// <home>/modes/<mode>.md wins, then the repo overlay is appended.
	ModePreamble string
	// Context Forge computed; the agent should not have to discover it.
	DeclaredChecks []string
	AttemptID      string
}

// assemblePrompt returns the hashable template (layers 1–2: preamble + overlay
// + autonomy block + routine prompt — never per-attempt values, DESIGN §9.1)
// and the rendered prompt (template + context block).
func assemblePrompt(in promptInput) (template, rendered string) {
	var t strings.Builder
	preamble := in.ModePreamble
	if in.Home != "" {
		if b, err := os.ReadFile(filepath.Join(in.Home, "modes", in.Mode+".md")); err == nil {
			// The live file is the versioned artifact proposals target (MODES.md).
			preamble = string(b)
		}
	}
	if preamble != "" {
		t.WriteString(strings.TrimRight(preamble, "\n"))
		t.WriteString("\n\n")
	}
	if in.RepoPath != "" {
		// The repo-scoped overlay: .forge/modes/<mode>.md inside the checkout,
		// read-only, appended after the global preamble.
		if b, err := os.ReadFile(filepath.Join(in.RepoPath, ".forge", "modes", in.Mode+".md")); err == nil {
			t.WriteString("REPOSITORY NOTES:\n")
			t.WriteString(strings.TrimRight(string(b), "\n"))
			t.WriteString("\n\n")
		}
	}
	if block := autonomyBlock(in.Autonomy, in.Checkpoints); block != "" {
		t.WriteString(block)
		t.WriteString("\n\n")
	}
	if in.RoutinePrompt != "" {
		// The label matters: every preamble ends with a "## Result" format
		// section, and an unlabeled task line after it reads as trailing
		// noise — haiku attempts repeatedly answered "no task was provided"
		// (M6 smoke 6/7) with the task sitting right there.
		t.WriteString("YOUR TASK (the routine prompt):\n")
		t.WriteString(strings.ReplaceAll(in.RoutinePrompt, "{{repo}}", in.Repository))
	}
	template = t.String()

	var c strings.Builder
	c.WriteString(template)
	fmt.Fprintf(&c, "\n\nCONTEXT (computed by Forge): repository %s; attempt %s; autonomy %s.", in.Repository, in.AttemptID, in.Autonomy)
	if len(in.DeclaredChecks) > 0 {
		fmt.Fprintf(&c, " Declared checks Forge will re-run: %s.", strings.Join(in.DeclaredChecks, ", "))
	}
	c.WriteString(" You are in an isolated git worktree; never push.")
	return template, c.String()
}

// autonomyBlock is the per-level instruction (MODES.md). The needs_input
// envelope is enforced by --json-schema for levels that allow questions.
func autonomyBlock(a model.Autonomy, checkpoints []string) string {
	const envelope = `end your final message with ONLY the JSON result object (no code fences); set needs_input to the question, options, and context. Your session resumes with the human's answer as the next message.`
	switch a {
	case model.AutonomyAsk:
		return "AUTONOMY: ask. When anything is ambiguous, before any irreversible step (a commit, deleting a file, changing a dependency), or if the task looks far larger than described, do not guess — " + envelope
	case model.AutonomyCheckpoint:
		list := "a declared checkpoint"
		if len(checkpoints) > 0 {
			list = "one of these checkpoints: " + strings.Join(checkpoints, ", ")
		}
		return "AUTONOMY: checkpoint. Decide small ambiguities yourself and proceed. Only at " + list + " or a genuinely blocking ambiguity, " + envelope
	case model.AutonomyNotify:
		return "AUTONOMY: notify. Decide and proceed; call forge_note_progress at each checkpoint; never return needs_input. State assumptions in your summary."
	case model.AutonomyAuto:
		return "AUTONOMY: auto. Decide and proceed; never return needs_input. State assumptions in your summary."
	}
	return ""
}

// briefMaxBytes caps what a repository brief adds to every attempt's system
// prompt (M11 repo briefs, DESIGN §22).
const briefMaxBytes = 4 << 10

// repoBrief renders the repository's kb brief for --append-system-prompt: the
// newest indexed note whose title starts with "brief: <repo>" (explore keeps
// note ids unique by suffixing the title, so a refresh is a new note). ""
// when none exists; every failure degrades to no brief — a claim must never
// fail on kb state.
func (s *Server) repoBrief(ctx context.Context, repo string) string {
	prefix := "brief: " + repo
	notes, err := s.store.SearchKb(ctx, "brief "+repo, 20)
	if err != nil {
		s.log.WarnContext(ctx, "repo brief search", "repository", repo, "error", err)
		return ""
	}
	var best *store.KbNote
	for i := range notes {
		if !strings.HasPrefix(notes[i].Title, prefix) {
			continue
		}
		if best == nil || notes[i].Created.After(best.Created) {
			best = &notes[i]
		}
	}
	if best == nil {
		return ""
	}
	n, err := kb.Parse(best.Path)
	if err != nil {
		s.log.WarnContext(ctx, "repo brief unreadable", "note", best.ID, "error", err)
		return ""
	}
	body := strings.TrimSpace(n.Body)
	if body == "" {
		return ""
	}
	head := fmt.Sprintf("REPOSITORY BRIEF for %s (kb note %s):\n", repo, best.ID)
	return head + cutBytes(body, briefMaxBytes-len(head))
}

// cutBytes cuts s to at most n bytes without splitting a UTF-8 sequence.
func cutBytes(s string, n int) string {
	if len(s) <= n {
		return s
	}
	for n > 0 && !utf8.RuneStart(s[n]) {
		n--
	}
	return s[:n]
}

// assembleClaimPrompt gathers assembly inputs from the claim's rows. It lives
// beside assemblePrompt so the claim builder stays one line.
func (s *Server) assembleClaimPrompt(ctx context.Context, tx *store.Tx, snap store.Routine, t store.Target, a *store.Attempt) (template, rendered string) {
	in := promptInput{Mode: snap.Mode, RoutinePrompt: snap.Prompt, Repository: t.Repository, Autonomy: a.Autonomy, Home: s.home, AttemptID: a.ID}
	if s.modes != nil {
		if m := s.modes.Get(snap.Mode); m != nil {
			in.ModePreamble = m.Preamble()
			in.Checkpoints = m.Checkpoints()
		}
	}
	repos, err := tx.Repositories(ctx)
	if err != nil {
		s.log.WarnContext(ctx, "prompt assembly: repositories", "error", err)
	}
	for _, r := range repos {
		if r.Name != t.Repository {
			continue
		}
		in.RepoPath = r.Path
		if r.ForgeToml == "" {
			continue
		}
		var ft struct {
			Checks map[string][]string `json:"Checks"`
		}
		if json.Unmarshal([]byte(r.ForgeToml), &ft) == nil {
			for name := range ft.Checks {
				in.DeclaredChecks = append(in.DeclaredChecks, name)
			}
			sort.Strings(in.DeclaredChecks)
		}
	}
	return assemblePrompt(in)
}
