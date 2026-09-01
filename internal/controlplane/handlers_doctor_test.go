package controlplane

import (
	"encoding/json"
	"net/http"
	"testing"

	"forge/internal/core/protocol"
	"forge/internal/doctor"
)

func TestDoctorEndpoint(t *testing.T) {
	h := newHarness(t, "unix")
	get := func() map[string]doctor.Check {
		t.Helper()
		status, raw := h.do(http.MethodGet, "/api/v1/doctor", nil, nil, "")
		if status != http.StatusOK {
			t.Fatalf("GET /api/v1/doctor = %d: %s", status, raw)
		}
		var checks []doctor.Check
		if err := json.Unmarshal(raw, &checks); err != nil {
			t.Fatalf("decode: %v", err)
		}
		byName := map[string]doctor.Check{}
		for _, c := range checks {
			byName[c.Name] = c
		}
		return byName
	}

	// A fresh daemon: schema known, no worker yet.
	fresh := get()
	if c := fresh["schema"]; c.Status != doctor.StatusOK || c.Detail == "" {
		t.Errorf("schema = %+v", c)
	}
	if c := fresh["worker"]; c.Status != doctor.StatusWarn {
		t.Errorf("worker = %+v", c)
	}
	if c := fresh["worktrees"]; c.Status != doctor.StatusOK {
		t.Errorf("worktrees = %+v", c)
	}

	// After a registration the worker rows and its capabilities appear.
	status, raw := h.do(http.MethodPost, "/api/v1/worker/register", protocol.RegisterRequest{
		WorkerID: testWorkerID, Name: "laptop", Version: "test", MaxConcurrent: 2, Executors: []string{"claude-code"},
		Capabilities: map[string]string{"executor:claude-code": "ready", "browser": "missing"},
		Repositories: []protocol.Repository{{Name: "app", Path: "/tmp/app", OriginIdentity: "example.com/app"}},
		Retained:     []protocol.RetainedWorktree{{AttemptID: otherWorker, Path: "/tmp/wt", Reason: "failed"}},
	}, nil, "")
	if status != http.StatusOK {
		t.Fatalf("register = %d: %s", status, raw)
	}
	after := get()
	if c := after["worker.laptop"]; c.Status != doctor.StatusOK {
		t.Errorf("worker.laptop = %+v", c)
	}
	if c := after["laptop.executor:claude-code"]; c.Status != doctor.StatusOK || c.Detail != "ready" {
		t.Errorf("executor = %+v", c)
	}
	if c := after["laptop.browser"]; c.Status != doctor.StatusWarn {
		t.Errorf("browser = %+v", c)
	}
	if c := after["repositories"]; c.Status != doctor.StatusOK {
		t.Errorf("repositories = %+v", c)
	}
	if c := after["worktrees"]; c.Status != doctor.StatusWarn {
		t.Errorf("worktrees = %+v", c)
	}
}
