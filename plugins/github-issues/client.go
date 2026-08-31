// client.go is the Forge side: a bearer-token HTTP client over the daemon's
// Unix socket and the minimal wire types this plugin reads and writes (its own
// copies — a plugin consumes the wire contract, not Forge's Go packages). The
// transport mirrors plugins/notify, the reference intake plugin.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
)

// defaultUI is where issue comments point humans; the daemon serves its UI on
// the loopback listener (DESIGN.md §1.1).
const defaultUI = "http://127.0.0.1:7340"

// createTaskRequest is the body of POST /api/v1/tasks.
type createTaskRequest struct {
	Prompt       string   `json:"prompt"`
	Repositories []string `json:"repositories"`
	Mode         string   `json:"mode"`
	Class        string   `json:"class"`
	Autonomy     string   `json:"autonomy"`
	Integrate    bool     `json:"integrate"`
	Model        string   `json:"model,omitempty"`
	Title        string   `json:"title"`
}

// createTaskResponse is the 201 body; only the work id is read.
type createTaskResponse struct {
	Work struct {
		ID string `json:"id"`
	} `json:"work"`
}

// externalRef is one link written by the annotate capability. The plugin omits
// the "plugin" field — the daemon fills it with this plugin's name.
type externalRef struct {
	Kind  string `json:"kind"`
	ID    string `json:"id"`
	URL   string `json:"url"`
	Label string `json:"label"`
}

// taskStatus is the slice of GET /api/v1/tasks/{id} this plugin needs: the
// work state and enough of the targets/attempts to name the fix branch and a
// result summary.
type taskStatus struct {
	State    string    `json:"state"`
	Targets  []target  `json:"targets"`
	Attempts []attempt `json:"attempts"`
}

type target struct {
	State  string `json:"state"`
	Branch string `json:"branch"`
}

type attempt struct {
	Branch string `json:"branch"`
	Head   string `json:"head"`
	Result struct {
		Summary string `json:"summary"`
	} `json:"result"`
}

// client talks to the daemon over FORGE_SOCKET with FORGE_TOKEN. The base URL
// host is a placeholder — the transport always dials the socket.
type client struct {
	hc    *http.Client
	token string
}

func newClient(socket, token string) *client {
	tr := &http.Transport{
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "unix", socket)
		},
	}
	return &client{hc: &http.Client{Transport: tr}, token: token}
}

// apiError is a non-2xx response.
type apiError struct {
	Status int
	Body   string
}

func (e *apiError) Error() string { return fmt.Sprintf("daemon returned %d: %s", e.Status, e.Body) }

func (c *client) do(ctx context.Context, method, path string, body, out any) (err error) {
	var rd io.Reader
	if body != nil {
		b, merr := json.Marshal(body)
		if merr != nil {
			return fmt.Errorf("encode %s %s: %w", method, path, merr)
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, "http://forge"+path, rd)
	if err != nil {
		return fmt.Errorf("build %s %s: %w", method, path, err)
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := c.hc.Do(req)
	if err != nil {
		return fmt.Errorf("%s %s: %w", method, path, err)
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close %s %s response: %w", method, path, cerr)
		}
	}()
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		msg, rerr := io.ReadAll(io.LimitReader(resp.Body, 4<<10))
		if rerr != nil {
			return &apiError{Status: resp.StatusCode, Body: rerr.Error()}
		}
		return &apiError{Status: resp.StatusCode, Body: strings.TrimSpace(string(msg))}
	}
	if out == nil {
		if _, derr := io.Copy(io.Discard, resp.Body); derr != nil {
			return fmt.Errorf("drain %s %s response: %w", method, path, derr)
		}
		return nil
	}
	if derr := json.NewDecoder(resp.Body).Decode(out); derr != nil {
		return fmt.Errorf("decode %s %s: %w", method, path, derr)
	}
	return nil
}

// CreateTask posts a new task and returns its work id.
func (c *client) CreateTask(ctx context.Context, req createTaskRequest) (string, error) {
	var out createTaskResponse
	if err := c.do(ctx, http.MethodPost, "/api/v1/tasks", req, &out); err != nil {
		return "", err
	}
	if out.Work.ID == "" {
		return "", fmt.Errorf("create task: daemon returned no work id")
	}
	return out.Work.ID, nil
}

// Annotate writes external_refs onto a Work; the daemon stamps the plugin name.
func (c *client) Annotate(ctx context.Context, workID string, refs []externalRef) error {
	body := map[string][]externalRef{"refs": refs}
	return c.do(ctx, http.MethodPost, "/api/v1/work/"+workID+"/external-refs", body, nil)
}

// TaskState reads a task's current state, targets, and attempts.
func (c *client) TaskState(ctx context.Context, workID string) (*taskStatus, error) {
	var out taskStatus
	if err := c.do(ctx, http.MethodGet, "/api/v1/tasks/"+workID, nil, &out); err != nil {
		return nil, err
	}
	return &out, nil
}
