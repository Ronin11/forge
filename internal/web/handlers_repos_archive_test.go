package web

import (
	"context"
	"net/http"
	"testing"

	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// Add / archive / restore: the Repos-page lifecycle. The filesystem work is a
// daemon hook, faked here; the handler wiring, the store writes, and the guards
// are what this exercises.
func TestRepositoryAddArchiveRestore(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID) // registers "equitizr"

	// Disabled by default (nil hooks): every route 400s, not 500s.
	for _, path := range []string{"/api/v1/repositories/equitizr/archive", "/api/v1/repositories/equitizr/restore"} {
		if status, _ := h.do(http.MethodPost, path, nil, nil, ""); status != http.StatusBadRequest {
			t.Fatalf("%s with nil hook = %d, want 400", path, status)
		}
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/repositories", addRepoRequest{Path: "/tmp/x"}, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("add with nil hook = %d, want 400", status)
	}

	// Inject fakes for the filesystem side.
	var addedArg, archivedArg, restoredArg string
	h.srv.addRepo = func(_ context.Context, path, url, name string) (protocol.Repository, error) {
		addedArg = path + url + name
		return protocol.Repository{Name: "newrepo", Path: "/tmp/newrepo"}, nil
	}
	h.srv.archiveRepo = func(_ context.Context, name, path string) (string, error) {
		archivedArg = name
		return "https://example.com/y.git", nil
	}
	h.srv.restoreRepo = func(_ context.Context, name, url string) (protocol.Repository, error) {
		restoredArg = name + "|" + url
		return protocol.Repository{Name: name}, nil
	}

	// Add: created, and the hook saw the URL.
	var pr protocol.Repository
	h.call(http.MethodPost, "/api/v1/repositories", addRepoRequest{URL: "file:///tmp/r.git"}, &pr, http.StatusCreated)
	if pr.Name != "newrepo" || addedArg != "file:///tmp/r.git" {
		t.Fatalf("add = %+v, hook saw %q", pr, addedArg)
	}
	// Add validation: neither path nor url.
	if status, _ := h.do(http.MethodPost, "/api/v1/repositories", addRepoRequest{}, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("empty add = %d, want 400", status)
	}

	// Archive an idle repo: 200, archived flag + captured origin persisted.
	var repo store.Repository
	h.call(http.MethodPost, "/api/v1/repositories/equitizr/archive", nil, &repo, http.StatusOK)
	if !repo.Archived || archivedArg != "equitizr" || repo.OriginURL != "https://example.com/y.git" {
		t.Fatalf("archive = %+v, hook saw %q", repo, archivedArg)
	}
	// Archiving again is refused.
	if status, _ := h.do(http.MethodPost, "/api/v1/repositories/equitizr/archive", nil, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("re-archive = %d, want 400", status)
	}

	// Restore: 200, archived cleared, hook got the saved URL.
	h.call(http.MethodPost, "/api/v1/repositories/equitizr/restore", nil, &repo, http.StatusOK)
	if repo.Archived || restoredArg != "equitizr|https://example.com/y.git" {
		t.Fatalf("restore = %+v, hook saw %q", repo, restoredArg)
	}
	// Restoring a non-archived repo is refused.
	if status, _ := h.do(http.MethodPost, "/api/v1/repositories/equitizr/restore", nil, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("restore non-archived = %d, want 400", status)
	}
}

// Archiving is refused while a target of the repository is running — deleting
// the checkout would break that attempt's linked worktrees.
func TestArchiveRefusesRunningWork(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.run("inventory")
	h.mustClaim("c1") // a target of equitizr is now running

	h.srv.archiveRepo = func(_ context.Context, name, path string) (string, error) { return "", nil }
	status, body := h.do(http.MethodPost, "/api/v1/repositories/equitizr/archive", nil, nil, "")
	if status != http.StatusBadRequest {
		t.Fatalf("archive with running work = %d, want 400 (%s)", status, body)
	}
}

// The app lifecycle endpoints: disabled by default, wired to the supervisor
// hook, and refusing an archived repository.
func TestRepositoryAppLifecycle(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID) // "equitizr"

	// Disabled (nil hooks): actions 400; status still answers stopped.
	for _, a := range []string{"start", "stop", "rebuild"} {
		if status, _ := h.do(http.MethodPost, "/api/v1/repositories/equitizr/app/"+a, nil, nil, ""); status != http.StatusBadRequest {
			t.Fatalf("app %s with nil hook = %d, want 400", a, status)
		}
	}
	var st protocol.AppStatus
	h.call(http.MethodGet, "/api/v1/repositories/equitizr/app", nil, &st, http.StatusOK)
	if st.State != "stopped" {
		t.Fatalf("nil-hook status = %+v", st)
	}

	// Wire fakes; every action returns the supervisor's status.
	var seen string
	mk := func(state string) func(context.Context, string, string) (protocol.AppStatus, error) {
		return func(_ context.Context, name, path string) (protocol.AppStatus, error) {
			seen = name + ":" + path
			return protocol.AppStatus{Configured: true, State: state, Port: 3001, URL: "http://localhost:3001"}, nil
		}
	}
	h.srv.startApp, h.srv.stopApp, h.srv.rebuildApp = mk("running"), mk("stopped"), mk("running")
	h.srv.appStatus = mk("running")

	h.call(http.MethodPost, "/api/v1/repositories/equitizr/app/start", nil, &st, http.StatusOK)
	if st.State != "running" || st.Port != 3001 || seen == "" {
		t.Fatalf("start = %+v (hook saw %q)", st, seen)
	}
	h.call(http.MethodPost, "/api/v1/repositories/equitizr/app/stop", nil, &st, http.StatusOK)
	if st.State != "stopped" {
		t.Fatalf("stop = %+v", st)
	}
	h.call(http.MethodGet, "/api/v1/repositories/equitizr/app", nil, &st, http.StatusOK)
	if !st.Configured || st.State != "running" {
		t.Fatalf("status = %+v", st)
	}
}
