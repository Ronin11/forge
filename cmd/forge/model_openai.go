package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"forge/internal/core/config"
)

// model_openai.go is the non-Anthropic half of the daemon's modelCall seam: one
// bounded chat completion against an OpenAI-compatible endpoint (DESIGN.md §21
// `kind = "openai-compatible"`). It is deliberately NOT a harness — no tools, no
// agentic loop, no session — because every modelCall consumer (attention
// auto-decide, the supervision watchdog, personas, the concierge, workflow
// drafts, experiments) wants exactly one system+user completion and parses the
// text itself.

// openAIModelTimeout is the default deadline for a local completion. The
// claude path's 60s suits a hosted frontier model; local inference is slower
// and may pay a cold model load on the first call after an idle period, so the
// local default is more generous. A caller with its own deadline keeps it.
const openAIModelTimeout = 4 * time.Minute

// openAIChatRequest is the subset of the chat-completions request Forge sends.
type openAIChatRequest struct {
	Model    string              `json:"model"`
	Messages []openAIChatMessage `json:"messages"`
	// Temperature is pinned to 0: every consumer of this seam is making a
	// decision or emitting structured JSON, not writing prose, and a sampled
	// answer to "may this question be auto-answered" is not reproducible
	// evidence. Change it here if a caller ever wants diversity.
	Temperature float64 `json:"temperature"`
	Stream      bool    `json:"stream"`
}

type openAIChatMessage struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

// openAIChatResponse is the subset of the reply Forge reads.
type openAIChatResponse struct {
	Choices []struct {
		Message struct {
			Content string `json:"content"`
		} `json:"message"`
		FinishReason string `json:"finish_reason"`
	} `json:"choices"`
	Error *struct {
		Message string `json:"message"`
	} `json:"error"`
}

// openAIModelCall runs one completion against the runner's endpoint and returns
// the assistant's text. info carries the resolved model id (the endpoint's own
// name for it, e.g. "qwen3-coder:30b") and rc the endpoint.
func openAIModelCall(ctx context.Context, rc config.RunnerConfig, info config.ModelInfo, system, user string) (reply string, err error) {
	if rc.Endpoint == "" {
		return "", fmt.Errorf("runner %s: no endpoint configured", info.Runner)
	}
	cctx, cancel := ctx, func() {}
	if _, has := ctx.Deadline(); !has {
		cctx, cancel = context.WithTimeout(ctx, openAIModelTimeout)
	}
	defer cancel()

	msgs := make([]openAIChatMessage, 0, 2)
	if strings.TrimSpace(system) != "" {
		msgs = append(msgs, openAIChatMessage{Role: "system", Content: system})
	}
	msgs = append(msgs, openAIChatMessage{Role: "user", Content: user})

	body, err := json.Marshal(openAIChatRequest{Model: info.ID, Messages: msgs, Temperature: 0, Stream: false})
	if err != nil {
		return "", fmt.Errorf("encode request: %w", err)
	}
	endpoint := strings.TrimSuffix(rc.Endpoint, "/") + "/chat/completions"
	req, err := http.NewRequestWithContext(cctx, http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return "", fmt.Errorf("build request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("%s: %w", info.Runner, err)
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("%s: close response: %w", info.Runner, cerr)
		}
	}()

	// Cap the read: a misconfigured endpoint streaming an unbounded body must
	// not balloon the daemon's memory.
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 8<<20))
	if err != nil {
		return "", fmt.Errorf("%s: read response: %w", info.Runner, err)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return "", fmt.Errorf("%s: http %d: %s", info.Runner, resp.StatusCode, snippet(raw))
	}
	var out openAIChatResponse
	if err := json.Unmarshal(raw, &out); err != nil {
		return "", fmt.Errorf("%s: parse response: %w: %s", info.Runner, err, snippet(raw))
	}
	if out.Error != nil && out.Error.Message != "" {
		return "", fmt.Errorf("%s: %s", info.Runner, out.Error.Message)
	}
	if len(out.Choices) == 0 {
		return "", fmt.Errorf("%s: no choices in response: %s", info.Runner, snippet(raw))
	}
	text := stripReasoning(out.Choices[0].Message.Content)
	if strings.TrimSpace(text) == "" {
		return "", fmt.Errorf("%s: empty completion (finish_reason %q)", info.Runner, out.Choices[0].FinishReason)
	}
	return text, nil
}

// stripReasoning removes <think>…</think> spans that reasoning models (qwen3,
// deepseek-r1) emit inline. Consumers such as parseAttentionDecision scan for
// the first '{' and the last '}', so a reasoning block that merely *discusses*
// the JSON — braces and all — makes the real answer unparseable. An unclosed
// block (the model hit its token cap mid-thought) drops the remainder, which is
// reasoning rather than answer either way.
func stripReasoning(s string) string {
	const open, close = "<think>", "</think>"
	for {
		i := strings.Index(s, open)
		if i < 0 {
			return strings.TrimSpace(s)
		}
		j := strings.Index(s[i:], close)
		if j < 0 {
			return strings.TrimSpace(s[:i])
		}
		s = s[:i] + s[i+j+len(close):]
	}
}

// snippet bounds an error message's echo of a response body.
func snippet(b []byte) string {
	const max = 300
	s := strings.TrimSpace(string(b))
	if len(s) > max {
		return s[:max] + "…"
	}
	return s
}
