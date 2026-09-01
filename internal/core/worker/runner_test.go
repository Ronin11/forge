package worker

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/logging"
	"forge/internal/core/protocol"
)

// TestNew_PartialRepositoryValidationFailure verifies that the worker starts
// successfully when some repositories fail validation, as long as at least one
// repository is valid. Invalid repositories are skipped with a warning.
func TestNew_PartialRepositoryValidationFailure(t *testing.T) {
	root := t.TempDir()
	cfg := filepath.Join(root, "gitconfig")
	if err := os.WriteFile(cfg, []byte("[user]\n\tname = Forge Test\n\temail = forge@example.invalid\n[init]\n\tdefaultBranch = master\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("GIT_CONFIG_GLOBAL", cfg)
	t.Setenv("GIT_CONFIG_NOSYSTEM", "1")

	// Create one valid repository.
	validRepo := filepath.Join(root, "valid")
	origin := filepath.Join(root, "origin.git")
	git := Git{}
	run := func(dir string, args ...string) {
		t.Helper()
		if _, err := git.Run(context.Background(), dir, args...); err != nil {
			t.Fatalf("git %s in %s: %v", strings.Join(args, " "), dir, err)
		}
	}
	run(root, "init", "--bare", "-b", "master", origin)
	run(root, "clone", origin, validRepo)
	if err := os.WriteFile(filepath.Join(validRepo, "README.md"), []byte("hello\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	run(validRepo, "add", "README.md")
	run(validRepo, "commit", "-q", "-m", "initial")
	run(validRepo, "push", "-q", "-u", "origin", "master")

	// Create directories that are NOT valid git repositories.
	invalidRepo1 := filepath.Join(root, "invalid1")
	if err := os.MkdirAll(invalidRepo1, 0o755); err != nil {
		t.Fatal(err)
	}
	invalidRepo2 := filepath.Join(root, "invalid2")
	if err := os.MkdirAll(invalidRepo2, 0o755); err != nil {
		t.Fatal(err)
	}
	// Initialize invalidRepo2 as a git repo but without an origin remote.
	run(invalidRepo2, "init", "-q")

	// Create a worker config with one valid and two invalid repositories.
	dataDir := filepath.Join(root, "data")
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		t.Fatal(err)
	}
	workerCfg := filepath.Join(root, "worker.toml")
	if _, err := WriteDefault(workerCfg, dataDir, "/bin/true"); err != nil {
		t.Fatal(err)
	}
	if err := AddRepository(workerCfg, "valid", validRepo, "master"); err != nil {
		t.Fatal(err)
	}
	if err := AddRepository(workerCfg, "invalid1", invalidRepo1, ""); err != nil {
		t.Fatal(err)
	}
	if err := AddRepository(workerCfg, "invalid2", invalidRepo2, ""); err != nil {
		t.Fatal(err)
	}

	cfg2, err := LoadConfig(workerCfg)
	if err != nil {
		t.Fatal(err)
	}

	// Create the worker. It should start successfully because at least one
	// repository is valid.
	w, err := New(context.Background(), WorkerOptions{
		Config:   cfg2,
		Version:  "test",
		Handler:  logging.Discard(),
		Clock:    time.Now,
		ForgeBin: "/bin/true",
		Daemon:   &testDaemon{},
	})
	if err != nil {
		t.Fatalf("New failed: %v", err)
	}
	defer w.Close()

	// Verify that only the valid repository is registered.
	repos := w.runner.repoList()
	if len(repos) != 1 {
		t.Errorf("got %d repositories, want 1", len(repos))
	}
	if len(repos) > 0 && repos[0].Name != "valid" {
		t.Errorf("got repository %q, want valid", repos[0].Name)
	}
}

// TestNew_AllRepositoriesInvalid verifies that the worker fails to start when
// all repositories fail validation and there is no greenfield projects_root.
func TestNew_AllRepositoriesInvalid(t *testing.T) {
	root := t.TempDir()

	// Create directories that are NOT valid git repositories.
	invalidRepo1 := filepath.Join(root, "invalid1")
	if err := os.MkdirAll(invalidRepo1, 0o755); err != nil {
		t.Fatal(err)
	}
	invalidRepo2 := filepath.Join(root, "invalid2")
	if err := os.MkdirAll(invalidRepo2, 0o755); err != nil {
		t.Fatal(err)
	}

	// Create a worker config with only invalid repositories.
	dataDir := filepath.Join(root, "data")
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		t.Fatal(err)
	}
	workerCfg := filepath.Join(root, "worker.toml")
	if _, err := WriteDefault(workerCfg, dataDir, "/bin/true"); err != nil {
		t.Fatal(err)
	}
	if err := AddRepository(workerCfg, "invalid1", invalidRepo1, ""); err != nil {
		t.Fatal(err)
	}
	if err := AddRepository(workerCfg, "invalid2", invalidRepo2, ""); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadConfig(workerCfg)
	if err != nil {
		t.Fatal(err)
	}

	// Attempt to create the worker. It should fail because no valid repositories
	// exist and there is no greenfield projects_root.
	_, err = New(context.Background(), WorkerOptions{
		Config:   cfg,
		Version:  "test",
		Handler:  logging.Discard(),
		Clock:    time.Now,
		ForgeBin: "/bin/true",
		Daemon:   &testDaemon{},
	})
	if err == nil {
		t.Fatal("New succeeded with all invalid repositories, want error")
	}
	if !strings.Contains(err.Error(), "no valid repositories configured") {
		t.Errorf("error = %v, want 'no valid repositories configured'", err)
	}
}

// testDaemon is a minimal daemon implementation for testing New.
type testDaemon struct {
	fakeDaemon
}

func (d *testDaemon) Register(ctx context.Context, req protocol.RegisterRequest) (*protocol.RegisterResponse, error) {
	return &protocol.RegisterResponse{}, nil
}

func (d *testDaemon) Claim(ctx context.Context, req protocol.ClaimRequest) (*protocol.Claim, error) {
	return nil, nil
}

func (d *testDaemon) PatchCleanup(ctx context.Context, attemptID string, p protocol.CleanupPatch) error {
	return nil
}

func (d *testDaemon) Attempt(ctx context.Context, attemptID string) (*AttemptState, error) {
	return nil, nil
}
