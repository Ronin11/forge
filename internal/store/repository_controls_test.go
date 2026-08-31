package store

import (
	"context"
	"errors"
	"testing"

	"forge/internal/protocol"
)

// registerDemo registers a worker advertising one repository, the fixture the
// repository-controls tests pause, resume, and re-register.
func registerDemo(t *testing.T, st *Store) {
	t.Helper()
	ctx := context.Background()
	req := protocol.RegisterRequest{WorkerID: "0123456789abcdef0123456789abcdef", Name: "laptop",
		Repositories: []protocol.Repository{{Name: "demo", Path: "/tmp/demo", OriginIdentity: "github.com/x/demo", Project: "default"}}}
	if err := st.Write(ctx, func(tx *Tx) error {
		if err := tx.EnsureProject(ctx, "default"); err != nil {
			return err
		}
		return tx.Register(ctx, req)
	}); err != nil {
		t.Fatal(err)
	}
}

func repoByName(t *testing.T, st *Store, name string) *Repository {
	t.Helper()
	r, err := st.Repository(context.Background(), name)
	if err != nil {
		t.Fatalf("read repository %s: %v", name, err)
	}
	return r
}

// A pause flag round-trips, journals the change, and survives the worker's
// re-registration (Register must not reset it).
func TestSetRepositoryPausedRoundTrip(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	registerDemo(t, st)

	if r := repoByName(t, st, "demo"); r.Paused {
		t.Fatalf("new repository should not be paused: %+v", r)
	}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.SetRepositoryPaused(ctx, "demo", true) }); err != nil {
		t.Fatal(err)
	}
	if r := repoByName(t, st, "demo"); !r.Paused {
		t.Fatalf("repository should be paused: %+v", r)
	}
	// A re-registration (the worker's heartbeat tick) must preserve paused.
	registerDemo(t, st)
	if r := repoByName(t, st, "demo"); !r.Paused {
		t.Fatalf("re-registration cleared paused: %+v", r)
	}
	// The journal recorded the pause under the daemon entity, keyed by name.
	hist, err := st.JournalForEntity(ctx, EntityDaemon, "demo")
	if err != nil {
		t.Fatal(err)
	}
	var paused bool
	for _, e := range hist {
		if e.Kind == "repository.paused" {
			paused = true
		}
	}
	if !paused {
		t.Errorf("no repository.paused journal row: %+v", hist)
	}
	// Resume clears it.
	if err := st.Write(ctx, func(tx *Tx) error { return tx.SetRepositoryPaused(ctx, "demo", false) }); err != nil {
		t.Fatal(err)
	}
	if r := repoByName(t, st, "demo"); r.Paused {
		t.Errorf("resume left paused set: %+v", r)
	}
}

func TestSetRepositoryPausedUnknown(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	err := st.Write(ctx, func(tx *Tx) error { return tx.SetRepositoryPaused(ctx, "nope", true) })
	if !errors.Is(err, ErrNotFound) {
		t.Fatalf("unknown repository = %v, want ErrNotFound", err)
	}
}

// The app-url round-trips, survives Register, and rejects nothing at the store
// layer (the API validates the value); the setter is symmetric with paused.
func TestSetRepositoryAppURLRoundTrip(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	registerDemo(t, st)

	if r := repoByName(t, st, "demo"); r.AppURL != "" {
		t.Fatalf("new repository should have no app url: %+v", r)
	}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.SetRepositoryAppURL(ctx, "demo", "https://localhost:3000") }); err != nil {
		t.Fatal(err)
	}
	if r := repoByName(t, st, "demo"); r.AppURL != "https://localhost:3000" {
		t.Fatalf("app url = %q", r.AppURL)
	}
	registerDemo(t, st)
	if r := repoByName(t, st, "demo"); r.AppURL != "https://localhost:3000" {
		t.Fatalf("re-registration cleared app url: %+v", r)
	}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.SetRepositoryAppURL(ctx, "demo", "") }); err != nil {
		t.Fatal(err)
	}
	if r := repoByName(t, st, "demo"); r.AppURL != "" {
		t.Errorf("clear left app url set: %+v", r)
	}
}

func TestSetRepositoryAppURLUnknown(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	err := st.Write(ctx, func(tx *Tx) error { return tx.SetRepositoryAppURL(ctx, "nope", "https://x") })
	if !errors.Is(err, ErrNotFound) {
		t.Fatalf("unknown repository = %v, want ErrNotFound", err)
	}
}
