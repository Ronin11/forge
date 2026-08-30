package worker

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"

	"forge/internal/protocol"
)

const requestTimeout = 15 * time.Second

// Client talks to the control plane's worker endpoints.
type Client struct {
	base  string
	token string
	http  *http.Client
}

// APIError is a non-2xx response.
type APIError struct {
	Status int
	Code   string
	Msg    string
}

func (e *APIError) Error() string {
	return fmt.Sprintf("control plane %d %s: %s", e.Status, e.Code, e.Msg)
}

// NewClient creates a client for base (e.g. http://127.0.0.1:7340).
func NewClient(base, token string) *Client {
	return &Client{base: base, token: token, http: &http.Client{Timeout: requestTimeout}}
}

func (c *Client) do(ctx context.Context, method, path string, in, out any) error {
	var body io.Reader
	if in != nil {
		b, err := json.Marshal(in)
		if err != nil {
			return err
		}
		body = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, c.base+path, body)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, 8<<20))
	if err != nil {
		return err
	}
	if resp.StatusCode/100 != 2 {
		var e protocol.ErrorResponse
		_ = json.Unmarshal(data, &e)
		if e.Error == "" {
			e.Error = string(bytes.TrimSpace(data))
		}
		return &APIError{Status: resp.StatusCode, Code: e.Code, Msg: e.Error}
	}
	if out != nil && len(data) > 0 {
		return json.Unmarshal(data, out)
	}
	return nil
}

// Register advertises the worker; it is repeated as a liveness signal.
func (c *Client) Register(ctx context.Context, req protocol.RegisterRequest) error {
	return c.do(ctx, http.MethodPost, "/api/v1/worker/register", req, nil)
}

// Claim asks for one Target. It returns nil, nil when nothing is pending.
func (c *Client) Claim(ctx context.Context, workerID string) (*protocol.Claim, error) {
	var claim protocol.Claim
	err := c.do(ctx, http.MethodPost, "/api/v1/worker/claim", protocol.ClaimRequest{WorkerID: workerID}, &claim)
	var apiErr *APIError
	if errors.As(err, &apiErr) && apiErr.Status == http.StatusNoContent {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if claim.Attempt.ID == "" {
		return nil, nil
	}
	return &claim, nil
}

// Heartbeat renews the lease and optionally advances the attempt state.
func (c *Client) Heartbeat(ctx context.Context, attemptID string, req protocol.HeartbeatRequest) (protocol.HeartbeatResponse, error) {
	var resp protocol.HeartbeatResponse
	err := c.do(ctx, http.MethodPost, "/api/v1/worker/attempts/"+attemptID+"/heartbeat", req, &resp)
	return resp, err
}

// Events appends a batch of events.
func (c *Client) Events(ctx context.Context, attemptID string, req protocol.EventsRequest) error {
	return c.do(ctx, http.MethodPost, "/api/v1/worker/attempts/"+attemptID+"/events", req, nil)
}

// Complete reports the terminal outcome (idempotent for cleanup updates).
func (c *Client) Complete(ctx context.Context, attemptID string, req protocol.CompleteRequest) error {
	return c.do(ctx, http.MethodPost, "/api/v1/worker/attempts/"+attemptID+"/complete", req, nil)
}
