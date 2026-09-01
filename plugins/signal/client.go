// client.go is the HTTP side: a bearer-token client over the daemon's Unix
// socket, the minimal view types this plugin reads (its own copies — a plugin
// consumes the wire contract, not Forge's Go packages), and the journal
// stream in both transports (SSE and the polling fallback). The shapes and
// the SSE framing mirror plugins/status-file, the reference events plugin.
package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"strconv"
	"strings"
	"time"
)

// defaultUI is where notification bodies point humans; the daemon serves its
// UI on the loopback listener (DESIGN.md §1.1).
const defaultUI = "http://127.0.0.1:7340"

// workView is the slice of a Work this plugin needs.
type workView struct {
	ID          string `json:"id"`
	RoutineName string `json:"routine_name"`
	Title       string `json:"title"`
}

// taskDetail is the slice of GET /api/v1/tasks/{id} this plugin needs.
type taskDetail struct {
	Work workView `json:"work"`
}

// queueItem is the slice of GET /api/v1/queue this plugin needs: the deferred
// reason is the throttle signal.
type queueItem struct {
	State  string `json:"state"`
	Reason string `json:"reason"`
}

// attentionView is GET /api/v1/attention; only questions are read (proposals
// arrive with their payload in the journal row).
type attentionView struct {
	Questions []questionView `json:"questions"`
}

type questionView struct {
	ID     string `json:"id"`
	WorkID string `json:"work_id"`
	Text   string `json:"text"`
}

// journalEntry is one row of GET /api/v1/journal — the audit-trail shape of
// docs/PLUGINS.md.
type journalEntry struct {
	ID         int64           `json:"id"`
	Time       time.Time       `json:"ts"`
	Kind       string          `json:"kind"`
	EntityType string          `json:"entity_type"`
	EntityID   string          `json:"entity_id"`
	Payload    json.RawMessage `json:"payload"`
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

// apiError is a non-2xx response, kept as a type so callers can branch on the
// status (404 → SSE fallback).
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

func (c *client) Task(ctx context.Context, id string) (*taskDetail, error) {
	var out taskDetail
	if err := c.do(ctx, http.MethodGet, "/api/v1/tasks/"+id, nil, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *client) Attention(ctx context.Context) (*attentionView, error) {
	var out attentionView
	if err := c.do(ctx, http.MethodGet, "/api/v1/attention", nil, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *client) Queue(ctx context.Context) ([]queueItem, error) {
	var out []queueItem
	if err := c.do(ctx, http.MethodGet, "/api/v1/queue", nil, &out); err != nil {
		return nil, err
	}
	return out, nil
}

// createTaskRequest is the body of POST /api/v1/tasks (intake).
type createTaskRequest struct {
	Prompt       string   `json:"prompt"`
	Repositories []string `json:"repositories"`
	Title        string   `json:"title,omitempty"`
	SubmittedBy  string   `json:"submitted_by,omitempty"`
}

type createTaskResponse struct {
	Work workView `json:"work"`
}

// CreateTask files arbitrary work; returns the new work id.
func (c *client) CreateTask(ctx context.Context, req createTaskRequest) (string, error) {
	var out createTaskResponse
	if err := c.do(ctx, http.MethodPost, "/api/v1/tasks", req, &out); err != nil {
		return "", err
	}
	return out.Work.ID, nil
}

// AnswerQuestion resolves a waiting question so its Work resumes.
func (c *client) AnswerQuestion(ctx context.Context, questionID, answer string) error {
	body := map[string]string{"answer": answer}
	return c.do(ctx, http.MethodPost, "/api/v1/questions/"+questionID+"/answer", body, nil)
}

// Assistant sends a message to the concierge and returns its reply.
func (c *client) Assistant(ctx context.Context, sender, text string) (string, error) {
	var out struct {
		Reply string `json:"reply"`
	}
	if err := c.do(ctx, http.MethodPost, "/api/v1/assistant/message", map[string]string{"sender": sender, "text": text}, &out); err != nil {
		return "", err
	}
	return out.Reply, nil
}

// Ack persists the plugin's journal cursor so a restart resumes with no gaps.
func (c *client) Ack(ctx context.Context, cursor int64) error {
	body := map[string]int64{"cursor": cursor}
	return c.do(ctx, http.MethodPost, "/api/v1/plugins/signal/ack", body, nil)
}

// Since is the polling read: rows with id > since, oldest first.
func (c *client) Since(ctx context.Context, since int64, limit int) ([]journalEntry, error) {
	var out []journalEntry
	path := "/api/v1/journal?since=" + strconv.FormatInt(since, 10) + "&limit=" + strconv.Itoa(limit)
	if err := c.do(ctx, http.MethodGet, path, nil, &out); err != nil {
		return nil, err
	}
	return out, nil
}

// errSSEUnsupported marks a daemon without the follow transport; the caller
// falls back to polling with the same cursor.
var errSSEUnsupported = errors.New("journal SSE not supported by this daemon")

// Follow opens GET /api/v1/journal?follow=1&since=N and emits each frame until
// the stream ends (nil), emit refuses, or the context is cancelled.
func (c *client) Follow(ctx context.Context, since int64, emit func(journalEntry) error) (err error) {
	url := "http://forge/api/v1/journal?follow=1&since=" + strconv.FormatInt(since, 10)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return fmt.Errorf("build journal follow request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("Accept", "text/event-stream")
	resp, err := c.hc.Do(req)
	if err != nil {
		return fmt.Errorf("open journal stream: %w", err)
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close journal stream: %w", cerr)
		}
	}()
	if resp.StatusCode == http.StatusNotFound {
		return errSSEUnsupported
	}
	if resp.StatusCode != http.StatusOK {
		msg, rerr := io.ReadAll(io.LimitReader(resp.Body, 4<<10))
		if rerr != nil {
			return &apiError{Status: resp.StatusCode, Body: rerr.Error()}
		}
		return &apiError{Status: resp.StatusCode, Body: strings.TrimSpace(string(msg))}
	}
	if ct := resp.Header.Get("Content-Type"); !strings.HasPrefix(ct, "text/event-stream") {
		return errSSEUnsupported
	}
	return parseSSE(resp.Body, emit)
}

// maxSSELine bounds one frame line; journal payloads are small, bounded
// structures (STYLE.md §6).
const maxSSELine = 4 << 20

// parseSSE reads frames of the daemon's journal stream: `event: journal`,
// `id: <journal id>`, `data: <entry JSON>`, blank line. Comment lines
// (keepalives) and other event types are skipped. EOF is a clean end (nil):
// the caller decides whether to reconnect.
func parseSSE(r io.Reader, emit func(journalEntry) error) error {
	sc := bufio.NewScanner(r)
	sc.Buffer(make([]byte, 64<<10), maxSSELine)
	event, data := "", []byte(nil)
	for sc.Scan() {
		line := sc.Text()
		switch {
		case line == "":
			if event == "journal" && len(data) > 0 {
				var e journalEntry
				if err := json.Unmarshal(data, &e); err != nil {
					return fmt.Errorf("decode journal frame: %w", err)
				}
				if err := emit(e); err != nil {
					return err
				}
			}
			event, data = "", nil
		case strings.HasPrefix(line, ":"):
			// keepalive comment
		case strings.HasPrefix(line, "event:"):
			event = strings.TrimSpace(strings.TrimPrefix(line, "event:"))
		case strings.HasPrefix(line, "data:"):
			d := strings.TrimPrefix(line, "data:")
			d = strings.TrimPrefix(d, " ")
			if len(data) > 0 {
				data = append(data, '\n')
			}
			data = append(data, d...)
		}
		// id: lines are informational — the entry itself carries its id.
	}
	if err := sc.Err(); err != nil {
		return fmt.Errorf("read journal stream: %w", err)
	}
	return nil
}
