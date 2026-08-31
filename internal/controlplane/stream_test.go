package controlplane

import (
	"bufio"
	"io"
	"net/http"
	"strconv"
	"strings"
	"testing"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
)

// postTestEvents stores a small worker batch on the attempt.
func postTestEvents(h *harness, attemptID string, msgs ...string) {
	h.t.Helper()
	events := make([]protocol.Event, len(msgs))
	for i, m := range msgs {
		events[i] = protocol.Event{Seq: i, Time: h.clock.Now().Add(time.Duration(i) * time.Second), Kind: protocol.KindLifecycle, Message: m}
	}
	h.call(http.MethodPost, "/api/v1/attempts/"+attemptID+"/events", protocol.EventBatch{Source: protocol.SourceWorker, Events: events}, nil, http.StatusOK)
}

// openStream GETs an SSE URL and returns the response; the caller closes it.
func openStream(t *testing.T, h *harness, path string) *http.Response {
	t.Helper()
	resp, err := h.http.Client().Get(h.http.URL + path)
	if err != nil {
		t.Fatal(err)
	}
	if resp.StatusCode != http.StatusOK {
		body, rerr := io.ReadAll(resp.Body)
		if rerr != nil {
			body = []byte(rerr.Error())
		}
		if err := resp.Body.Close(); err != nil {
			t.Error(err)
		}
		t.Fatalf("GET %s = %d %s", path, resp.StatusCode, body)
	}
	return resp
}

// sseBlocks feeds each blank-line-terminated SSE block down a channel, closed
// at EOF, so tests can wait for specific frames with a deadline.
func sseBlocks(r io.Reader) <-chan string {
	ch := make(chan string, 64)
	go func() {
		defer close(ch)
		br := bufio.NewReader(r)
		var block strings.Builder
		for {
			line, err := br.ReadString('\n')
			if line == "\n" && block.Len() > 0 {
				ch <- block.String()
				block.Reset()
			} else if line != "\n" {
				block.WriteString(line)
			}
			if err != nil {
				return
			}
		}
	}()
	return ch
}

// nextBlock waits for one block matching all substrings, skipping others.
func nextBlock(t *testing.T, ch <-chan string, want ...string) string {
	t.Helper()
	deadline := time.After(5 * time.Second)
	for {
		select {
		case block, ok := <-ch:
			if !ok {
				t.Fatalf("stream closed before a block with %q", want)
			}
			matched := true
			for _, w := range want {
				if !strings.Contains(block, w) {
					matched = false
					break
				}
			}
			if matched {
				return block
			}
		case <-deadline:
			t.Fatalf("no block with %q within 5s", want)
		}
	}
}

func TestWorkStreamTerminalWorkEndsAndResumes(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.srv.streamInterval = 10 * time.Millisecond
	h.register(testWorkerID)
	h.createRoutine("inventory")
	wc := h.run("inventory")
	c := h.mustClaim("r1")
	postTestEvents(h, c.AttemptID, "hello from agent", "second line")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 1)
	h.complete(c, completeRequest(model.Succeeded, h.clock.Now()))

	resp := openStream(t, h, "/api/v1/work/"+wc.Work.ID+"/stream")
	body, err := io.ReadAll(resp.Body) // the work is terminal: the server ends the stream
	if cerr := resp.Body.Close(); cerr != nil {
		t.Error(cerr)
	}
	if err != nil {
		t.Fatal(err)
	}
	got := string(body)
	for _, want := range []string{"event: journal", "event: attempt", "hello from agent", "second line", "event: end", `"state":"succeeded"`} {
		if !strings.Contains(got, want) {
			t.Errorf("stream body lacks %q:\n%s", want, got)
		}
	}
	if ct := resp.Header.Get("Content-Type"); ct != "text/event-stream" {
		t.Errorf("Content-Type = %q", ct)
	}

	// ids are ascending journal ids; resume from the last one sees no journal
	// rows, just the end event.
	last := int64(-1)
	for _, line := range strings.Split(got, "\n") {
		if raw, ok := strings.CutPrefix(line, "id: "); ok {
			n, err := strconv.ParseInt(raw, 10, 64)
			if err != nil {
				t.Fatalf("id line %q: %v", line, err)
			}
			if n < last {
				t.Errorf("id %d after %d", n, last)
			}
			last = n
		}
	}
	if last <= 0 {
		t.Fatalf("no id lines in stream:\n%s", got)
	}
	resp = openStream(t, h, "/api/v1/tasks/"+wc.Work.ID+"/stream?since="+strconv.FormatInt(last, 10))
	body, err = io.ReadAll(resp.Body)
	if cerr := resp.Body.Close(); cerr != nil {
		t.Error(cerr)
	}
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(body), "event: journal") {
		t.Errorf("resume from %d repeated journal rows:\n%s", last, body)
	}
	if !strings.Contains(string(body), "event: end") {
		t.Errorf("resume lacks end event:\n%s", body)
	}
}

func TestWorkStreamFollowsToTheEnd(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.srv.streamInterval = 10 * time.Millisecond
	h.register(testWorkerID)
	h.createRoutine("inventory")
	wc := h.run("inventory")

	resp := openStream(t, h, "/api/v1/work/"+wc.Work.ID+"/stream")
	defer func() {
		if err := resp.Body.Close(); err != nil && !strings.Contains(err.Error(), "closed") {
			t.Error(err)
		}
	}()
	blocks := sseBlocks(resp.Body)
	nextBlock(t, blocks, "event: journal") // creation rows arrive while the work is open

	c := h.mustClaim("r1")
	postTestEvents(h, c.AttemptID, "streamed live")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 1)
	h.complete(c, completeRequest(model.Succeeded, h.clock.Now()))

	nextBlock(t, blocks, "event: attempt", "streamed live")
	nextBlock(t, blocks, "event: end", `"state":"succeeded"`)
	if _, ok := <-blocks; ok {
		// drain to close; any trailing block would be a bug
		t.Error("stream kept sending after the end event")
	}
}

func TestWorkStreamDrainClosesWithRetryHint(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.srv.streamInterval = 10 * time.Millisecond
	h.register(testWorkerID)
	h.createRoutine("inventory")
	wc := h.run("inventory")

	resp := openStream(t, h, "/api/v1/work/"+wc.Work.ID+"/stream")
	blocks := sseBlocks(resp.Body)
	nextBlock(t, blocks, "event: journal")
	h.srv.SetDraining(false) // no-op, keeps the ordering below honest
	h.srv.SetDraining(true)
	nextBlock(t, blocks, "retry: 2000")
	select {
	case _, ok := <-blocks:
		if ok {
			t.Error("stream kept sending after the retry hint")
		}
	case <-time.After(5 * time.Second):
		t.Error("stream not closed after the retry hint")
	}
	if err := resp.Body.Close(); err != nil {
		t.Error(err)
	}
}

func TestWorkStreamOnceStopsAtCatchUp(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.srv.streamInterval = 10 * time.Millisecond
	h.register(testWorkerID)
	h.createRoutine("inventory")
	wc := h.run("inventory") // stays open: nothing claims it

	resp := openStream(t, h, "/api/v1/work/"+wc.Work.ID+"/stream?once=1")
	body, err := io.ReadAll(resp.Body)
	if cerr := resp.Body.Close(); cerr != nil {
		t.Error(cerr)
	}
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(body), "event: journal") {
		t.Errorf("once pass lacks journal rows:\n%s", body)
	}
	if strings.Contains(string(body), "event: end") {
		t.Errorf("once pass of an open work claims it ended:\n%s", body)
	}
}

func TestWorkStreamRejectsBadRequests(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	if status, _ := h.do(http.MethodGet, "/api/v1/work/not-an-id/stream", nil, nil, ""); status != http.StatusBadRequest {
		t.Errorf("bad id = %d", status)
	}
	if status, _ := h.do(http.MethodGet, "/api/v1/work/"+testWorkerID+"/stream", nil, nil, ""); status != http.StatusNotFound {
		t.Errorf("unknown work = %d", status)
	}
	h.createRoutine("inventory")
	wc := h.run("inventory")
	if status, _ := h.do(http.MethodGet, "/api/v1/work/"+wc.Work.ID+"/stream?since=x", nil, nil, ""); status != http.StatusBadRequest {
		t.Errorf("bad since = %d", status)
	}
}
