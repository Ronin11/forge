package worker

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"time"

	"forge/internal/core/protocol"
)

// Client is the worker's view of the daemon: a thin HTTP client over the Unix
// socket (no token) or loopback TCP (worker token).
type Client struct {
	http    *http.Client
	baseURL string
	token   string
}

// ErrDaemonDown wraps transport failures so callers can back off rather than
// treat them as verdicts.
var ErrDaemonDown = errors.New("daemon unreachable")

// StatusError is a non-2xx response with the daemon's message.
type StatusError struct {
	Status  int
	Message string
}

func (e *StatusError) Error() string {
	return fmt.Sprintf("daemon returned %d: %s", e.Status, e.Message)
}

// NewClient builds a client for "unix://<path>" or "http://host:port".
func NewClient(daemon, token string, timeout time.Duration) (*Client, error) {
	if timeout == 0 {
		timeout = 10 * time.Second
	}
	transport := &http.Transport{MaxIdleConns: 4, IdleConnTimeout: 90 * time.Second}
	c := &Client{token: token, http: &http.Client{Transport: transport, Timeout: timeout}}
	switch {
	case strings.HasPrefix(daemon, "unix://"):
		sock := strings.TrimPrefix(daemon, "unix://")
		transport.DialContext = func(ctx context.Context, _, _ string) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "unix", sock)
		}
		c.baseURL = "http://forge"
	case strings.HasPrefix(daemon, "http://"):
		c.baseURL = strings.TrimRight(daemon, "/")
	default:
		return nil, fmt.Errorf("daemon address %q: want unix:// or http://", daemon)
	}
	return c, nil
}

// do performs one JSON request. A nil out discards the body.
func (c *Client) do(ctx context.Context, method, path string, in, out any) (err error) {
	var body io.Reader
	if in != nil {
		b, err := json.Marshal(in)
		if err != nil {
			return fmt.Errorf("encode %s %s: %w", method, path, err)
		}
		body = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, c.baseURL+path, body)
	if err != nil {
		return fmt.Errorf("build %s %s: %w", method, path, err)
	}
	if in != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	if c.token != "" {
		req.Header.Set("Authorization", "Bearer "+c.token)
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return fmt.Errorf("%s %s: %w: %v", method, path, ErrDaemonDown, err)
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close response body: %w", cerr)
		}
	}()
	if resp.StatusCode == http.StatusNoContent {
		// An empty answer (no claimable work); there is no body to decode.
		return nil
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		var e protocol.Error
		msg, rerr := io.ReadAll(io.LimitReader(resp.Body, 4<<10))
		if rerr != nil {
			return &StatusError{Status: resp.StatusCode, Message: "unreadable error body: " + rerr.Error()}
		}
		if json.Unmarshal(msg, &e) == nil && e.Error != "" {
			return &StatusError{Status: resp.StatusCode, Message: e.Error}
		}
		return &StatusError{Status: resp.StatusCode, Message: strings.TrimSpace(string(msg))}
	}
	if out == nil {
		if _, err := io.Copy(io.Discard, resp.Body); err != nil {
			return fmt.Errorf("drain %s %s: %w", method, path, err)
		}
		return nil
	}
	if err := json.NewDecoder(resp.Body).Decode(out); err != nil {
		return fmt.Errorf("decode %s %s: %w", method, path, err)
	}
	return nil
}

// Handshake reads the daemon's version and state.
func (c *Client) Handshake(ctx context.Context) (*protocol.Handshake, error) {
	var h protocol.Handshake
	if err := c.do(ctx, http.MethodGet, "/api/v1/handshake", nil, &h); err != nil {
		return nil, err
	}
	return &h, nil
}

// Register advertises the worker.
func (c *Client) Register(ctx context.Context, req protocol.RegisterRequest) (*protocol.RegisterResponse, error) {
	var out protocol.RegisterResponse
	if err := c.do(ctx, http.MethodPost, "/api/v1/worker/register", req, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

// Claim asks for one Target; nil, nil means the queue had nothing admissible.
func (c *Client) Claim(ctx context.Context, req protocol.ClaimRequest) (*protocol.Claim, error) {
	var out protocol.Claim
	err := c.do(ctx, http.MethodPost, "/api/v1/worker/claim", req, &out)
	var se *StatusError
	if errors.As(err, &se) && se.Status == http.StatusNoContent {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if out.AttemptID == "" {
		return nil, nil
	}
	return &out, nil
}

// Heartbeat renews the lease and reports progress.
func (c *Client) Heartbeat(ctx context.Context, attemptID string, req protocol.HeartbeatRequest) (*protocol.HeartbeatResponse, error) {
	var out protocol.HeartbeatResponse
	if err := c.do(ctx, http.MethodPost, "/api/v1/attempts/"+attemptID+"/heartbeat", req, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

// Events sends one batch.
func (c *Client) Events(ctx context.Context, attemptID string, batch protocol.EventBatch) error {
	return c.do(ctx, http.MethodPost, "/api/v1/attempts/"+attemptID+"/events", batch, nil)
}

// Complete sends the terminal report.
func (c *Client) Complete(ctx context.Context, attemptID string, req protocol.CompleteRequest) (*protocol.CompleteResponse, error) {
	var out protocol.CompleteResponse
	if err := c.do(ctx, http.MethodPost, "/api/v1/attempts/"+attemptID+"/complete", req, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

// PatchCleanup updates cleanup/git fields after reconcile.
func (c *Client) PatchCleanup(ctx context.Context, attemptID string, p protocol.CleanupPatch) error {
	return c.do(ctx, http.MethodPatch, "/api/v1/attempts/"+attemptID+"/cleanup", p, nil)
}

// AttemptState is what reconcile needs to know about the daemon's view.
type AttemptState struct {
	AttemptID   string `json:"attempt_id"`
	TargetID    string `json:"target_id"`
	TargetState string `json:"target_state"`
	Terminal    bool   `json:"terminal"`
	Resumable   bool   `json:"resumable"`
}

// Attempt reads the daemon's view of an attempt (reconcile step 3).
func (c *Client) Attempt(ctx context.Context, attemptID string) (*AttemptState, error) {
	var out AttemptState
	if err := c.do(ctx, http.MethodGet, "/api/v1/worker/attempts/"+attemptID, nil, &out); err != nil {
		return nil, err
	}
	return &out, nil
}
