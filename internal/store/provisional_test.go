package store

import (
	"context"
	"testing"

	"forge/internal/protocol"
)

// A provisional row (registered on the fly, DESIGN §1.3) has no worker; the
// worker's next Register upsert claims it.
func TestUpsertProvisionalRepository(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	rep := protocol.Repository{Name: "flyrepo", Path: "/tmp/x", OriginIdentity: "github.com/a/b"}
	if err := st.Write(ctx, func(tx *Tx) error {
		if err := tx.EnsureProject(ctx, "default"); err != nil {
			return err
		}
		return tx.UpsertProvisionalRepository(ctx, rep)
	}); err != nil {
		t.Fatal(err)
	}
	repos, err := st.Repositories(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var got *Repository
	for i := range repos {
		if repos[i].Name == "flyrepo" {
			got = &repos[i]
		}
	}
	if got == nil || got.WorkerID != "" || got.Path != "/tmp/x" || got.Project != "default" {
		t.Fatalf("provisional = %+v", got)
	}
	req := protocol.RegisterRequest{WorkerID: "0123456789abcdef0123456789abcdef", Name: "w",
		Repositories: []protocol.Repository{{Name: "flyrepo", Path: "/tmp/x", OriginIdentity: "github.com/a/b"}}}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.Register(ctx, req) }); err != nil {
		t.Fatal(err)
	}
	repos, err = st.Repositories(ctx)
	if err != nil {
		t.Fatal(err)
	}
	for _, r := range repos {
		if r.Name == "flyrepo" && r.WorkerID != req.WorkerID {
			t.Errorf("worker_id = %q", r.WorkerID)
		}
	}
}
