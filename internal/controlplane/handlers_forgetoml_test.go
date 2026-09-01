package controlplane

import (
	"context"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

func registerWith(h *harness, repos ...protocol.Repository) {
	h.t.Helper()
	req := protocol.RegisterRequest{WorkerID: testWorkerID, Name: "laptop", Version: "test", MaxConcurrent: 2, Executors: []string{"claude-code"}, Repositories: repos}
	h.call(http.MethodPost, "/api/v1/worker/register", req, nil, http.StatusOK)
}

func mustWrite(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}

// Registration of a repository without forge.toml files one doc proposal with
// a guessed skeleton, exactly once; a repository with the file gets none.
func TestForgeTomlBootstrapProposal(t *testing.T) {
	h := newHarness(t, transportUnix)
	npmRepo := t.TempDir()
	mustWrite(t, filepath.Join(npmRepo, "package.json"), `{"scripts":{"test":"jest","lint":"eslint ."}}`)
	goRepo := t.TempDir()
	mustWrite(t, filepath.Join(goRepo, "go.mod"), "module example.com/gopher\n")
	covered := t.TempDir()
	mustWrite(t, filepath.Join(covered, "forge.toml"), "[checks]\n")

	repos := []protocol.Repository{
		{Name: "npmrepo", Path: npmRepo, OriginIdentity: "x/npmrepo", Project: "default"},
		{Name: "gopher", Path: goRepo, OriginIdentity: "x/gopher", Project: "default"},
		{Name: "covered", Path: covered, OriginIdentity: "x/covered", Project: "default"},
	}
	registerWith(h, repos...)

	byTarget := func() map[string]store.Proposal {
		ps, err := h.st.ListProposals(context.Background(), "")
		if err != nil {
			t.Fatal(err)
		}
		out := map[string]store.Proposal{}
		for _, p := range ps {
			out[p.Target] = p
		}
		return out
	}
	ps := byTarget()
	if len(ps) != 2 {
		t.Fatalf("proposals = %+v, want npmrepo and gopher only", ps)
	}
	npm, ok := ps["repo:npmrepo/forge.toml"]
	if !ok || npm.Kind != model.ProposalDoc || npm.Status != model.ProposalProposed {
		t.Fatalf("npmrepo proposal = %+v", npm)
	}
	if after := string(npm.After); !strings.Contains(after, "npm") || !strings.Contains(after, "lint") || !strings.Contains(after, "test") {
		t.Errorf("npmrepo skeleton = %s", after)
	}
	if gop := ps["repo:gopher/forge.toml"]; !strings.Contains(string(gop.After), "go") {
		t.Errorf("gopher skeleton = %s", gop.After)
	}
	if _, ok := ps["repo:covered/forge.toml"]; ok {
		t.Error("repository with forge.toml got a proposal")
	}

	// The next registration tick creates nothing new.
	registerWith(h, repos...)
	if again := byTarget(); len(again) != 2 {
		t.Errorf("second register duplicated proposals: %+v", again)
	}

	// A rejection is final: registration does not resurrect the proposal.
	err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		_, derr := tx.DecideProposal(context.Background(), npm.ID, model.ProposalRejected, "human")
		return derr
	})
	if err != nil {
		t.Fatal(err)
	}
	registerWith(h, repos...)
	if again := byTarget(); len(again) != 2 || again["repo:npmrepo/forge.toml"].Status != model.ProposalRejected {
		t.Errorf("register after rejection = %+v", again)
	}
}
