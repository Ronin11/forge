// teams.go is the outbound boundary: turning a notification into the JSON an
// incoming webhook accepts and POSTing it. Two shapes are supported because
// Teams has two: the Adaptive Card envelope a Power Automate "post to a channel
// when a webhook request is received" workflow expects (the default), and the
// legacy Office 365 connector MessageCard. Both are plain JSON over HTTPS — no
// SDK, no auth beyond the URL's own secret.
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
)

// postTimeout bounds one webhook POST; a slow channel must never wedge the
// journal loop.
const postTimeout = 20 * time.Second

// note is one outbound notification, independent of the card shape: a headline,
// a body, optional label/value facts, and where a human should click.
type note struct {
	Title  string
	Text   string
	URL    string
	Facts  [][2]string
	Urgent bool // renders attention-colored
}

// webhook posts notes to one Teams incoming webhook URL.
type webhook struct {
	url    string
	format string
	hc     *http.Client
}

func newWebhook(url, format string) *webhook {
	return &webhook{url: url, format: format, hc: &http.Client{Timeout: postTimeout}}
}

// post delivers one note. An error is returned, never fatal — a missed card
// must not kill the plugin.
func (w *webhook) post(ctx context.Context, n note) (err error) {
	payload := adaptivePayload(n)
	if w.format == formatMessageCard {
		payload = messageCardPayload(n)
	}
	body, err := json.Marshal(payload)
	if err != nil {
		return fmt.Errorf("encode teams card: %w", err)
	}
	cctx, cancel := context.WithTimeout(ctx, postTimeout)
	defer cancel()
	req, err := http.NewRequestWithContext(cctx, http.MethodPost, w.url, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("build teams webhook request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := w.hc.Do(req)
	if err != nil {
		return fmt.Errorf("post teams webhook: %w", err)
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close teams webhook response: %w", cerr)
		}
	}()
	msg, rerr := io.ReadAll(io.LimitReader(resp.Body, 4<<10))
	if rerr != nil {
		return fmt.Errorf("read teams webhook response: %w", rerr)
	}
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return fmt.Errorf("teams webhook returned %d: %s", resp.StatusCode, strings.TrimSpace(string(msg)))
	}
	return nil
}

// adaptivePayload is the message envelope Teams workflows accept: one Adaptive
// Card attachment. Text is carried in TextBlocks (wrap on, so long prompts do
// not clip) and the link becomes a button.
func adaptivePayload(n note) map[string]any {
	color := "Default"
	if n.Urgent {
		color = "Attention"
	}
	body := []any{
		map[string]any{"type": "TextBlock", "text": n.Title, "weight": "Bolder", "size": "Medium", "wrap": true, "color": color},
	}
	if n.Text != "" {
		body = append(body, map[string]any{"type": "TextBlock", "text": n.Text, "wrap": true})
	}
	if len(n.Facts) > 0 {
		facts := make([]any, 0, len(n.Facts))
		for _, f := range n.Facts {
			facts = append(facts, map[string]any{"title": f[0], "value": f[1]})
		}
		body = append(body, map[string]any{"type": "FactSet", "facts": facts})
	}
	card := map[string]any{
		"$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
		"type":    "AdaptiveCard",
		"version": "1.4",
		"body":    body,
	}
	if n.URL != "" {
		card["actions"] = []any{map[string]any{"type": "Action.OpenUrl", "title": "Open in Forge", "url": n.URL}}
	}
	return map[string]any{
		"type": "message",
		"attachments": []any{map[string]any{
			"contentType": "application/vnd.microsoft.card.adaptive",
			"contentUrl":  nil,
			"content":     card,
		}},
	}
}

// messageCardPayload is the legacy connector shape, for webhooks created before
// Office 365 connectors were retired.
func messageCardPayload(n note) map[string]any {
	color := "5B6670"
	if n.Urgent {
		color = "C4314B"
	}
	card := map[string]any{
		"@type":      "MessageCard",
		"@context":   "https://schema.org/extensions",
		"summary":    n.Title,
		"themeColor": color,
		"title":      n.Title,
		"text":       n.Text,
	}
	if len(n.Facts) > 0 {
		facts := make([]any, 0, len(n.Facts))
		for _, f := range n.Facts {
			facts = append(facts, map[string]any{"name": f[0], "value": f[1]})
		}
		card["sections"] = []any{map[string]any{"facts": facts}}
	}
	if n.URL != "" {
		card["potentialAction"] = []any{map[string]any{
			"@type":   "OpenUri",
			"name":    "Open in Forge",
			"targets": []any{map[string]any{"os": "default", "uri": n.URL}},
		}}
	}
	return card
}
