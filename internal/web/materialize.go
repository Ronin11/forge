package web

// materializeRoutine is the ONE place a routine's executable content comes
// together before freezing or previewing: directive resolution (a target
// routine's content lives in the library), per-call overrides, persona
// composition, and {{objective}} injection. createWorkTx, renderPreview, and
// the experiments' variant previews all run through it, so a preview is
// byte-faithful to a run by construction.

import (
	"forge/internal/core/directives"
	"forge/internal/core/store"
)

// materializeOpts are the per-call overrides, strongest first. Precedence:
// per-call override > directive frontmatter > persona default (model only) >
// routine row (Objective only).
type materializeOpts struct {
	Mode, Model, Prompt, Persona, Objective string
	// Lib overrides the daemon's library — the optimization loop's variant
	// composition. Nil uses the live library.
	Lib *directives.Library
	// Assign enrolls this materialization in a live experiment when one
	// covers the resolved directive or persona (live_experiments.go). Only
	// root production works set it — previews and continuations never do.
	Assign bool
}

func (s *Server) materializeLibrary(opts materializeOpts) *directives.Library {
	if opts.Lib != nil {
		return opts.Lib
	}
	return s.promptLibrary()
}

// materializeRoutine fills rt's content fields in place and returns the
// composition manifest (nil only for a legacy/ad-hoc routine with no persona
// and no directive). A workflow-target routine has no prompt to materialize —
// the caller creates a workflow run instead; asking for one here is an error.
func (s *Server) materializeRoutine(rt *store.Routine, opts materializeOpts) (*directives.Composition, error) {
	lib := s.materializeLibrary(opts)
	var comp *directives.Composition
	// Live-experiment assignment: substitute the arm's library BEFORE
	// directive resolution so the directive body and any persona both
	// compose from the same arm. Control stamps but keeps the base library.
	expID, variant := "", ""
	if opts.Assign && opts.Lib == nil {
		var armLib *directives.Library
		armLib, expID, variant = s.assignLiveExperiment(rt, opts, lib)
		if armLib != nil {
			lib = armLib
		}
	}

	kind, name, err := store.ParseTarget(rt.Target)
	if err != nil {
		return nil, badRequest("routine %s: %v", rt.Name, err)
	}
	switch kind {
	case store.TargetWorkflow:
		return nil, badRequest("routine %s targets workflow %q — it runs as a workflow run, not a Work", rt.Name, name)
	case store.TargetScript:
		return nil, badRequest("routine %s targets script %q — it runs as a script run, not a Work", rt.Name, name)
	case store.TargetDirective:
		if lib == nil {
			return nil, badRequest("routine %s targets directive %q but this process has no library", rt.Name, name)
		}
		d := lib.Directive(name)
		if d == nil {
			return nil, badRequest("routine %s: directive %q is not in the library (directives/%s.md)", rt.Name, name, name)
		}
		body, manifest, err := lib.ResolveDirectiveBody(name)
		if err != nil {
			return nil, badRequest("%v", err)
		}
		rt.Mode, rt.Prompt, rt.Effort = d.Mode, body, d.Effort
		rt.Persona, rt.Model = d.PersonaRef, d.Model
		comp = &directives.Composition{Mode: d.Mode, Commit: lib.Commit, Dirty: lib.Dirty, Fragments: manifest}
	}

	// Per-call overrides.
	if opts.Mode != "" {
		rt.Mode = opts.Mode
	}
	if opts.Model != "" {
		rt.Model = opts.Model
	}
	if opts.Prompt != "" {
		rt.Prompt = opts.Prompt
	}
	if opts.Persona != "" {
		rt.Persona = opts.Persona
	}

	// Persona composition: resolved text ahead of the prompt, default model
	// where the routine left it empty — the snapshot and prompt_hash carry
	// the exact bytes.
	if rt.Persona != "" {
		if lib == nil {
			return nil, badRequest("routine %s names persona %q but this process has no prompts library", rt.Name, rt.Persona)
		}
		text, pcomp, err := lib.Resolve(rt.Persona, rt.Mode)
		if err != nil {
			return nil, badRequest("%v", err)
		}
		if rt.Model == "" {
			rt.Model = lib.Persona(rt.Persona).Model
		}
		if text != "" {
			rt.Prompt = text + "\n\n" + rt.Prompt
		}
		comp = mergeCompositions(comp, &pcomp)
	}

	rt.Prompt = injectObjective(rt.Prompt, firstNonEmpty(opts.Objective, rt.Objective))
	if comp != nil && expID != "" {
		comp.Experiment, comp.Variant = expID, variant
	}
	return comp, nil
}

// mergeCompositions folds the persona manifest into a directive's (fragments
// deduplicated); either side may be nil.
func mergeCompositions(directive, persona *directives.Composition) *directives.Composition {
	if directive == nil {
		return persona
	}
	if persona == nil {
		return directive
	}
	out := *persona // persona's identity fields (Persona, Mode, Commit, Dirty) win
	seen := map[string]bool{}
	for _, e := range out.Fragments {
		seen[e.Name] = true
	}
	for _, e := range directive.Fragments {
		if !seen[e.Name] {
			out.Fragments = append(out.Fragments, e)
		}
	}
	return &out
}

// targetOf parses a stored routine's target. Rows validate at save, so a
// malformed value (impossible short of hand-edited SQL) reads as legacy.
func targetOf(rt *store.Routine) (store.TargetKind, string) {
	kind, name, err := store.ParseTarget(rt.Target)
	if err != nil {
		return "", ""
	}
	return kind, name
}

func firstNonEmpty(a, b string) string {
	if a != "" {
		return a
	}
	return b
}
