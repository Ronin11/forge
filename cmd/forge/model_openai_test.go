package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"forge/internal/core/config"
)

// chatServer stands in for an OpenAI-compatible endpoint, capturing the request
// and replying with the given assistant content.
func chatServer(t *testing.T, content string, got *openAIChatRequest) *httptest.Server {
	t.Helper()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/chat/completions" {
			t.Errorf("path = %q, want /v1/chat/completions", r.URL.Path)
		}
		if got != nil {
			body, rerr := io.ReadAll(r.Body)
			if rerr != nil {
				t.Errorf("read request body: %v", rerr)
			}
			if err := json.Unmarshal(body, got); err != nil {
				t.Errorf("decode request: %v", err)
			}
		}
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprint(w, `{"choices":[{"message":{"role":"assistant","content":`+
			mustJSONString(content)+`},"finish_reason":"stop"}]}`)
	}))
	t.Cleanup(srv.Close)
	return srv
}

func mustJSONString(s string) string {
	b, err := json.Marshal(s)
	if err != nil {
		panic(err)
	}
	return string(b)
}

func localModel(endpoint string) (config.RunnerConfig, config.ModelInfo) {
	return config.RunnerConfig{Kind: "openai-compatible", Billing: "local", Endpoint: endpoint + "/v1"},
		config.ModelInfo{Alias: "local", ID: "qwen3-coder:30b", Runner: "local", RunnerKind: "openai-compatible"}
}

// TestOpenAIModelCallRequestShape pins what Forge sends: the resolved model id
// (not the alias), system and user as separate messages, non-streaming, and
// temperature 0 so a decision is reproducible.
func TestOpenAIModelCallRequestShape(t *testing.T) {
	var got openAIChatRequest
	srv := chatServer(t, `{"answer":"yes"}`, &got)
	rc, info := localModel(srv.URL)

	out, err := openAIModelCall(context.Background(), rc, info, "be terse", "the question")
	if err != nil {
		t.Fatalf("openAIModelCall: %v", err)
	}
	if out != `{"answer":"yes"}` {
		t.Errorf("content = %q", out)
	}
	if got.Model != "qwen3-coder:30b" {
		t.Errorf("model = %q, want the resolved id qwen3-coder:30b", got.Model)
	}
	if got.Stream {
		t.Error("stream = true, want false: the seam returns one whole string")
	}
	if got.Temperature != 0 {
		t.Errorf("temperature = %v, want 0", got.Temperature)
	}
	if len(got.Messages) != 2 ||
		got.Messages[0].Role != "system" || got.Messages[0].Content != "be terse" ||
		got.Messages[1].Role != "user" || got.Messages[1].Content != "the question" {
		t.Errorf("messages = %+v", got.Messages)
	}
}

// TestOpenAIModelCallOmitsEmptySystem keeps an empty system prompt out of the
// message list rather than sending a blank turn.
func TestOpenAIModelCallOmitsEmptySystem(t *testing.T) {
	var got openAIChatRequest
	srv := chatServer(t, "ok", &got)
	rc, info := localModel(srv.URL)

	if _, err := openAIModelCall(context.Background(), rc, info, "   ", "hi"); err != nil {
		t.Fatalf("openAIModelCall: %v", err)
	}
	if len(got.Messages) != 1 || got.Messages[0].Role != "user" {
		t.Errorf("messages = %+v, want the user turn only", got.Messages)
	}
}

// TestOpenAIModelCallStripsReasoning is the case that matters for the attention
// decider: a reasoning model narrates about the JSON before emitting it, and
// parseAttentionDecision takes the first '{' and the last '}'. Without
// stripping, the answer parsed is the one inside the thought.
func TestOpenAIModelCallStripsReasoning(t *testing.T) {
	reply := "<think>Maybe {\"answer\":\"no\"} is right, but actually no.</think>{\"answer\":\"yes\"}"
	srv := chatServer(t, reply, nil)
	rc, info := localModel(srv.URL)

	out, err := openAIModelCall(context.Background(), rc, info, "", "q")
	if err != nil {
		t.Fatalf("openAIModelCall: %v", err)
	}
	if out != `{"answer":"yes"}` {
		t.Errorf("content = %q, want the post-reasoning JSON only", out)
	}
}

func TestStripReasoning(t *testing.T) {
	for _, tc := range []struct{ name, in, want string }{
		{"none", `{"a":1}`, `{"a":1}`},
		{"leading block", "<think>hmm</think>answer", "answer"},
		{"trailing whitespace", "<think>hmm</think>\n\n  answer  ", "answer"},
		{"multiple blocks", "<think>a</think>x<think>b</think>y", "xy"},
		{"unclosed drops remainder", "answer<think>still thinking...", "answer"},
		{"only reasoning", "<think>all thought</think>", ""},
		{"braces inside reasoning", `<think>{"answer":"no"}</think>{"answer":"yes"}`, `{"answer":"yes"}`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := stripReasoning(tc.in); got != tc.want {
				t.Errorf("stripReasoning(%q) = %q, want %q", tc.in, got, tc.want)
			}
		})
	}
}

// TestOpenAIModelCallErrors covers the failure modes a local endpoint actually
// produces, each of which must surface as an error rather than an empty answer
// the caller would treat as a decision.
func TestOpenAIModelCallErrors(t *testing.T) {
	for _, tc := range []struct {
		name    string
		handler http.HandlerFunc
		want    string
	}{
		{
			name: "http error",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				w.WriteHeader(http.StatusInternalServerError)
				fmt.Fprint(w, `{"error":{"message":"model not found"}}`)
			},
			want: "http 500",
		},
		{
			name: "error field",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				fmt.Fprint(w, `{"error":{"message":"context length exceeded"}}`)
			},
			want: "context length exceeded",
		},
		{
			name: "no choices",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				fmt.Fprint(w, `{"choices":[]}`)
			},
			want: "no choices",
		},
		{
			name: "empty completion",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				fmt.Fprint(w, `{"choices":[{"message":{"content":""},"finish_reason":"length"}]}`)
			},
			want: "empty completion",
		},
		{
			name: "reasoning-only completion",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				fmt.Fprint(w, `{"choices":[{"message":{"content":"<think>ran out of room"},"finish_reason":"length"}]}`)
			},
			want: "empty completion",
		},
		{
			name: "unparseable body",
			handler: func(w http.ResponseWriter, _ *http.Request) {
				fmt.Fprint(w, `<html>proxy error</html>`)
			},
			want: "parse response",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			srv := httptest.NewServer(tc.handler)
			defer srv.Close()
			rc, info := localModel(srv.URL)
			_, err := openAIModelCall(context.Background(), rc, info, "", "q")
			if err == nil {
				t.Fatalf("want an error containing %q, got nil", tc.want)
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error = %v, want it to contain %q", err, tc.want)
			}
		})
	}
}

// TestOpenAIModelCallNoEndpoint fails fast rather than dialling "".
func TestOpenAIModelCallNoEndpoint(t *testing.T) {
	_, err := openAIModelCall(context.Background(), config.RunnerConfig{Kind: "openai-compatible"},
		config.ModelInfo{Runner: "local", ID: "m"}, "", "q")
	if err == nil || !strings.Contains(err.Error(), "no endpoint") {
		t.Errorf("error = %v, want a no-endpoint error", err)
	}
}

// TestOpenAIModelCallHonoursCallerDeadline checks the caller's context wins:
// the prompt-optimization loop sets its own longer deadline and must keep it,
// and a cancelled context must not hang for the local default.
func TestOpenAIModelCallHonoursCallerDeadline(t *testing.T) {
	// The handler blocks until the test releases it: waiting on the request
	// context instead would leave Close() blocked on an in-flight handler when
	// the client hangs up, which deadlocks the test rather than failing it.
	release := make(chan struct{})
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		select {
		case <-release:
		case <-r.Context().Done():
		}
	}))
	defer srv.Close()
	defer close(release)
	rc, info := localModel(srv.URL)

	ctx, cancel := context.WithTimeout(context.Background(), 150*time.Millisecond)
	defer cancel()
	start := time.Now()
	if _, err := openAIModelCall(ctx, rc, info, "", "q"); err == nil {
		t.Fatal("want a deadline error")
	}
	if elapsed := time.Since(start); elapsed > 30*time.Second {
		t.Errorf("took %v: the caller's deadline was ignored", elapsed)
	}
}

// TestOpenAIModelCallTrailingSlashEndpoint tolerates a config endpoint written
// with a trailing slash, which would otherwise build a //chat/completions path.
func TestOpenAIModelCallTrailingSlashEndpoint(t *testing.T) {
	srv := chatServer(t, "ok", nil)
	rc, info := localModel(srv.URL)
	rc.Endpoint += "/"

	if _, err := openAIModelCall(context.Background(), rc, info, "", "q"); err != nil {
		t.Fatalf("openAIModelCall with trailing slash: %v", err)
	}
}
