package tools_test

import (
	"errors"
	"strings"
	"testing"

	"forge/internal/model"
	"forge/internal/store"
	"forge/internal/tools"
)

func TestPropose(t *testing.T) {
	f := newFixture(t)
	w, tg, a := f.attempt(model.AutonomyAuto, "r1")
	att := toolAttempt(w, tg, a)
	out := f.mustCall("forge_propose", att, `{"kind":"process","target":"routine:inventory",
		"before":{"timeout_seconds":300},"after":{"timeout_seconds":120},
		"rationale":"p95 is 40s; a 300s timeout hides hangs","verification_plan":"watch the next 5 runs for timeouts"}`)
	id := str(t, out["id"])
	if out["status"] != "proposed" {
		t.Errorf("status = %v, want proposed", out["status"])
	}
	p := must(f.s.GetProposal(ctx(), id))
	if p.Source != "attempt:"+a.ID {
		t.Errorf("source = %q, want attempt:%s", p.Source, a.ID)
	}
	if p.Kind != model.ProposalProcess || p.Target != "routine:inventory" || p.Status != model.ProposalProposed {
		t.Errorf("proposal = %+v", p)
	}
	if !strings.Contains(string(p.Before), "300") || !strings.Contains(string(p.After), "120") {
		t.Errorf("before/after = %s / %s", p.Before, p.After)
	}
	// source_note names the kb note the proposal came from, replacing the
	// attempt default.
	out = f.mustCall("forge_propose", att,
		`{"kind":"doc","target":"kb:retro-findings","rationale":"fold the findings in","verification_plan":"note exists and links the attempts","source_note":"kb:retro-2026-08-30"}`)
	p2 := must(f.s.GetProposal(ctx(), str(t, out["id"])))
	if p2.Source != "kb:retro-2026-08-30" {
		t.Errorf("source = %q, want the source_note", p2.Source)
	}
	fn := must(f.s.Funnel(ctx()))
	if fn.Proposed != 2 || fn.Approved != 0 {
		t.Errorf("funnel = %+v, want 2 proposed", fn)
	}
}

func TestProposeRefusals(t *testing.T) {
	f := newFixture(t)
	w, tg, a := f.attempt(model.AutonomyAuto, "r1")
	att := toolAttempt(w, tg, a)
	for name, input := range map[string]string{
		"bad kind":      `{"kind":"vibe","target":"x","rationale":"r","verification_plan":"v"}`,
		"no kind":       `{"target":"x","rationale":"r","verification_plan":"v"}`,
		"no target":     `{"kind":"doc","rationale":"r","verification_plan":"v"}`,
		"no rationale":  `{"kind":"doc","target":"x","verification_plan":"v"}`,
		"no plan":       `{"kind":"doc","target":"x","rationale":"r"}`,
		"unknown field": `{"kind":"doc","target":"x","rationale":"r","verification_plan":"v","bogus":1}`,
	} {
		if _, err := f.call("forge_propose", att, input); !tools.IsBadInput(err) {
			t.Errorf("%s = %v, want BadInput", name, err)
		}
	}
	// Constitution 8: the store refuses the target with a conflict, not a 400.
	if _, err := f.call("forge_propose", att, `{"kind":"doc","target":"docs/CONSTITUTION.md","rationale":"r","verification_plan":"v"}`); !errors.Is(err, store.ErrConflict) {
		t.Errorf("constitution target = %v, want ErrConflict", err)
	}
	if list := must(f.s.ListProposals(ctx(), "")); len(list) != 0 {
		t.Errorf("a refused proposal persisted: %+v", list)
	}
}
