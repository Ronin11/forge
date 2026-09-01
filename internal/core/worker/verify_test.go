package worker

import (
	"strings"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// mkEnv builds an envelope claiming exactly the given changed paths.
func mkEnv(changes ...string) *ResultEnvelope {
	e := &ResultEnvelope{SchemaVersion: 1, Summary: "s"}
	for _, p := range changes {
		e.Changes = append(e.Changes, ResultChange{Path: p, Kind: "modified"})
	}
	return e
}

func TestVerifyScopesAndLevels(t *testing.T) {
	cases := []struct {
		name        string
		env         *ResultEnvelope
		git         protocol.GitOutcome
		checks      []CheckResult
		declared    bool
		scope       model.WriteScope
		globs       []string
		wantPass    bool
		wantLevel   int
		wantReason  string
		wantVerdict string // substring of the verdict JSON, "" to skip
	}{
		{
			name: "repo clean passes vacuously", env: mkEnv(), scope: model.WritesRepo,
			wantPass: true, wantLevel: 1, wantVerdict: `"l1_vacuous":true`,
		},
		{
			name: "repo allows any paths", env: mkEnv("src/main.go"),
			git:      protocol.GitOutcome{Commits: 1, ChangedPaths: []string{"src/main.go"}},
			scope:    model.WritesRepo,
			wantPass: true, wantLevel: 1,
		},
		{
			name: "changes mismatch stops at level 0", env: mkEnv("a.go"),
			git:      protocol.GitOutcome{ChangedPaths: []string{"b.go"}},
			scope:    model.WritesRepo,
			wantPass: false, wantLevel: 0, wantReason: "l0:changes_mismatch",
			wantVerdict: `"l0_changed_not_claimed":["b.go"]`,
		},
		{
			name: "none dirty fails scope", env: mkEnv("x.go"),
			git:      protocol.GitOutcome{Dirty: true, ChangedPaths: []string{"x.go"}},
			scope:    model.WritesNone,
			wantPass: false, wantLevel: 0, wantReason: "l0:scope:none",
			wantVerdict: `"l0_scope_offending"`,
		},
		{
			name: "kb_only with commits fails scope", env: mkEnv(),
			git:      protocol.GitOutcome{Commits: 2},
			scope:    model.WritesKbOnly,
			wantPass: false, wantLevel: 0, wantReason: "l0:scope:kb_only",
		},
		{
			name: "kb_only clean passes", env: mkEnv(), scope: model.WritesKbOnly,
			wantPass: true, wantLevel: 1,
		},
		{
			name: "docs_only default globs pass", env: mkEnv("docs/a/b.md", "README.md"),
			git:      protocol.GitOutcome{Commits: 1, ChangedPaths: []string{"docs/a/b.md", "README.md"}},
			scope:    model.WritesDocsOnly,
			wantPass: true, wantLevel: 1,
		},
		{
			name: "docs_only default globs do not cross directories", env: mkEnv("nested/notes.md"),
			git:      protocol.GitOutcome{ChangedPaths: []string{"nested/notes.md"}},
			scope:    model.WritesDocsOnly,
			wantPass: false, wantLevel: 0, wantReason: "l0:scope:docs_only",
			wantVerdict: `"l0_scope_offending":["nested/notes.md"]`,
		},
		{
			name: "docs_only declared globs pass", env: mkEnv("manual/x.txt"),
			git:   protocol.GitOutcome{ChangedPaths: []string{"manual/x.txt"}},
			scope: model.WritesDocsOnly, globs: []string{"manual/**"},
			wantPass: true, wantLevel: 1,
		},
		{
			name: "docs_only offending path fails", env: mkEnv("docs/a.md", "src/main.go"),
			git:      protocol.GitOutcome{ChangedPaths: []string{"docs/a.md", "src/main.go"}},
			scope:    model.WritesDocsOnly,
			wantPass: false, wantLevel: 0, wantReason: "l0:scope:docs_only",
			wantVerdict: `"l0_scope_offending":["src/main.go"]`,
		},
		{
			name: "new_project unrestricted", env: mkEnv("anything/at/all"),
			git:      protocol.GitOutcome{Commits: 3, ChangedPaths: []string{"anything/at/all"}},
			scope:    model.WritesNewProject,
			wantPass: true, wantLevel: 1,
		},
		{
			name:     "claim without evidence fails L0",
			env:      &ResultEnvelope{SchemaVersion: 1, Claims: []ResultClaim{{Claim: "it works", Evidence: " "}}},
			scope:    model.WritesRepo,
			wantPass: false, wantLevel: 0, wantReason: "l0:claims_evidence",
		},
		{
			name: "declared check failure is L1", env: mkEnv(),
			checks:   []CheckResult{{Check: "test", Passed: false}},
			declared: true, scope: model.WritesRepo,
			wantPass: false, wantLevel: 1, wantReason: "check_failed:test",
		},
		{
			name:     "false claim is check_claim_mismatch",
			env:      &ResultEnvelope{SchemaVersion: 1, ChecksRun: []ResultCheckRun{{Check: "test", Passed: true}}},
			checks:   []CheckResult{{Check: "test", Passed: false}},
			declared: true, scope: model.WritesRepo,
			wantPass: false, wantLevel: 1, wantReason: "check_claim_mismatch:test",
		},
		{
			name:     "unclaimed check is one-directional",
			env:      &ResultEnvelope{SchemaVersion: 1},
			checks:   []CheckResult{{Check: "test", Passed: true}, {Check: "lint", Passed: true}},
			declared: true, scope: model.WritesRepo,
			wantPass: true, wantLevel: 1,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			v := Verify(tc.env, tc.env != nil, tc.git, tc.checks, tc.declared, tc.scope, tc.globs)
			if v.Passed != tc.wantPass || v.Level != tc.wantLevel || v.Reason != tc.wantReason {
				t.Errorf("Verify = passed %v level %d reason %q, want %v %d %q", v.Passed, v.Level, v.Reason, tc.wantPass, tc.wantLevel, tc.wantReason)
			}
			if tc.wantVerdict != "" && !strings.Contains(string(v.Verdict), tc.wantVerdict) {
				t.Errorf("verdict %s lacks %s", v.Verdict, tc.wantVerdict)
			}
		})
	}
}

func TestGlobMatch(t *testing.T) {
	cases := []struct {
		glob, path string
		want       bool
	}{
		{"docs/**", "docs/a.md", true},
		{"docs/**", "docs/deep/nest/a.md", true},
		{"docs/**", "docs", true},
		{"docs/**", "docsx/a.md", false},
		{"*.md", "README.md", true},
		{"*.md", "docs/a.md", false},
		{"manual/*.txt", "manual/a.txt", true},
		{"manual/*.txt", "manual/sub/a.txt", false},
		{"[", "anything", false},
	}
	for _, tc := range cases {
		if got := globMatch(tc.glob, tc.path); got != tc.want {
			t.Errorf("globMatch(%q, %q) = %v, want %v", tc.glob, tc.path, got, tc.want)
		}
	}
}
