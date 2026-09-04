// graph.go is both boundaries for mode "graph": sending and reading mail
// through Microsoft Graph with an app-only token. This is the path for a
// Microsoft 365 / Outlook mailbox, where basic-auth SMTP and IMAP are usually
// disabled. It needs an Entra app registration with the application permissions
// Mail.Send and Mail.ReadWrite, admin-consented and (best practice) scoped to
// the one mailbox with an application access policy.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"html"
	"io"
	"net/http"
	"net/url"
	"regexp"
	"strings"
	"sync"
	"time"
)

const (
	// graphBase is the Graph endpoint; loginBase mints tokens.
	graphBase = "https://graph.microsoft.com/v1.0"
	loginBase = "https://login.microsoftonline.com"
	// tokenSkew renews a token this long before it actually expires.
	tokenSkew = 2 * time.Minute
	// graphTimeout bounds one Graph call.
	graphTimeout = 30 * time.Second
)

// graphMail sends and reads one mailbox. The mutex guards the cached token and
// its expiry, the only shared state.
type graphMail struct {
	cfg graphConfig
	to  string
	hc  *http.Client
	now func() time.Time
	// Endpoints, injected so tests can point them at a local server; production
	// always uses graphBase and loginBase.
	base  string
	login string

	mu      sync.Mutex
	token   string
	expires time.Time
}

func newGraphMail(cfg graphConfig, to string, now func() time.Time) *graphMail {
	return &graphMail{cfg: cfg, to: to, hc: &http.Client{Timeout: graphTimeout}, now: now, base: graphBase, login: loginBase}
}

// accessToken returns a valid bearer token, minting a new one when the cached
// one is missing or close to expiry.
func (g *graphMail) accessToken(ctx context.Context) (string, error) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.token != "" && g.now().Before(g.expires.Add(-tokenSkew)) {
		return g.token, nil
	}
	form := url.Values{
		"client_id":     {g.cfg.ClientID},
		"client_secret": {g.cfg.ClientSecret},
		"scope":         {"https://graph.microsoft.com/.default"},
		"grant_type":    {"client_credentials"},
	}
	endpoint := g.login + "/" + url.PathEscape(g.cfg.TenantID) + "/oauth2/v2.0/token"
	var out struct {
		AccessToken string `json:"access_token"`
		ExpiresIn   int64  `json:"expires_in"`
	}
	req, err := http.NewRequest(http.MethodPost, endpoint, strings.NewReader(form.Encode()))
	if err != nil {
		return "", fmt.Errorf("build token request: %w", err)
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	if err := g.do(ctx, req, &out); err != nil {
		return "", fmt.Errorf("mint graph token: %w", err)
	}
	if out.AccessToken == "" {
		return "", fmt.Errorf("mint graph token: response carried no access_token")
	}
	g.token, g.expires = out.AccessToken, g.now().Add(time.Duration(out.ExpiresIn)*time.Second)
	return g.token, nil
}

// send posts one message through the mailbox. The plugin header rides along so
// a copy landing back in the inbox is recognised as Forge's own.
func (g *graphMail) send(ctx context.Context, msg outgoing) error {
	body := map[string]any{
		"message": map[string]any{
			"subject": msg.Subject,
			"body":    map[string]any{"contentType": "Text", "content": msg.Body},
			"toRecipients": []any{map[string]any{
				"emailAddress": map[string]any{"address": addressOf(g.to)},
			}},
			"internetMessageHeaders": []any{map[string]any{"name": pluginHeader, "value": "email"}},
		},
		"saveToSentItems": true,
	}
	endpoint := g.base + "/users/" + url.PathEscape(g.cfg.Mailbox) + "/sendMail"
	if err := g.call(ctx, http.MethodPost, endpoint, body, nil); err != nil {
		return fmt.Errorf("graph sendMail: %w", err)
	}
	return nil
}

// graphMessageList is the slice of a Graph mail response this plugin reads.
type graphMessageList struct {
	Value []struct {
		ID                string `json:"id"`
		Subject           string `json:"subject"`
		InternetMessageID string `json:"internetMessageId"`
		Body              struct {
			ContentType string `json:"contentType"`
			Content     string `json:"content"`
		} `json:"body"`
		From struct {
			EmailAddress struct {
				Address string `json:"address"`
			} `json:"emailAddress"`
		} `json:"from"`
		InternetMessageHeaders []struct {
			Name  string `json:"name"`
			Value string `json:"value"`
		} `json:"internetMessageHeaders"`
	} `json:"value"`
}

// fetch returns unread mail and marks it read, mirroring the IMAP path.
func (g *graphMail) fetch(ctx context.Context) ([]inboundMail, error) {
	endpoint := fmt.Sprintf("%s/users/%s/mailFolders/inbox/messages?$filter=isRead%%20eq%%20false&$top=%d&$select=id,subject,from,body,internetMessageId,internetMessageHeaders",
		g.base, url.PathEscape(g.cfg.Mailbox), fetchLimit)
	var list graphMessageList
	if err := g.call(ctx, http.MethodGet, endpoint, nil, &list); err != nil {
		return nil, fmt.Errorf("graph read inbox: %w", err)
	}
	var msgs []inboundMail
	for _, m := range list.Value {
		body := m.Body.Content
		if strings.EqualFold(m.Body.ContentType, "html") {
			body = htmlToText(body)
		}
		msg := inboundMail{
			ID: m.ID, From: m.From.EmailAddress.Address, Subject: m.Subject,
			Body: body, MessageID: m.InternetMessageID,
		}
		for _, h := range m.InternetMessageHeaders {
			switch {
			case strings.EqualFold(h.Name, pluginHeader),
				strings.EqualFold(h.Name, "Auto-Submitted") && !strings.EqualFold(h.Value, "no"),
				strings.EqualFold(h.Name, "List-Id"):
				msg.Auto = true
			}
		}
		mark := g.base + "/users/" + url.PathEscape(g.cfg.Mailbox) + "/messages/" + url.PathEscape(m.ID)
		if err := g.call(ctx, http.MethodPatch, mark, map[string]any{"isRead": true}, nil); err != nil {
			return msgs, fmt.Errorf("mark message %s read: %w", m.ID, err)
		}
		msgs = append(msgs, msg)
	}
	return msgs, nil
}

// call performs one authenticated JSON request, decoding into out when given.
func (g *graphMail) call(ctx context.Context, method, endpoint string, body, out any) error {
	token, err := g.accessToken(ctx)
	if err != nil {
		return err
	}
	var rd io.Reader
	if body != nil {
		b, merr := json.Marshal(body)
		if merr != nil {
			return fmt.Errorf("encode %s body: %w", method, merr)
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequest(method, endpoint, rd)
	if err != nil {
		return fmt.Errorf("build %s request: %w", method, err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	return g.do(ctx, req, out)
}

// do sends a prepared request under a timeout and decodes a JSON response.
func (g *graphMail) do(ctx context.Context, req *http.Request, out any) (err error) {
	cctx, cancel := context.WithTimeout(ctx, graphTimeout)
	defer cancel()
	resp, err := g.hc.Do(req.WithContext(cctx))
	if err != nil {
		return fmt.Errorf("%s %s: %w", req.Method, req.URL.Path, err)
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close %s response: %w", req.URL.Path, cerr)
		}
	}()
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		msg, rerr := io.ReadAll(io.LimitReader(resp.Body, 4<<10))
		if rerr != nil {
			return fmt.Errorf("graph returned %d (body unreadable: %w)", resp.StatusCode, rerr)
		}
		return fmt.Errorf("graph returned %d: %s", resp.StatusCode, strings.TrimSpace(string(msg)))
	}
	if out == nil {
		if _, derr := io.Copy(io.Discard, resp.Body); derr != nil {
			return fmt.Errorf("drain %s response: %w", req.URL.Path, derr)
		}
		return nil
	}
	if derr := json.NewDecoder(resp.Body).Decode(out); derr != nil {
		return fmt.Errorf("decode %s response: %w", req.URL.Path, derr)
	}
	return nil
}

// Outlook bodies are HTML. Only two edits matter for reading a reply: block
// ends become newlines, every other tag is dropped.
var (
	htmlBreak = regexp.MustCompile(`(?i)<(br\s*/?|/p|/div|/li|/tr)>`)
	htmlTag   = regexp.MustCompile(`(?s)<(script|style)\b.*?</(script|style)>|<[^>]*>`)
)

// htmlToText renders an HTML mail body as the text a human typed.
func htmlToText(body string) string {
	s := htmlBreak.ReplaceAllString(body, "\n")
	s = htmlTag.ReplaceAllString(s, "")
	s = html.UnescapeString(s)
	s = strings.ReplaceAll(s, "\u00a0", " ") // non-breaking spaces Outlook inserts
	lines := strings.Split(s, "\n")
	kept := lines[:0]
	for _, ln := range lines {
		kept = append(kept, strings.TrimSpace(ln))
	}
	return strings.TrimSpace(strings.Join(kept, "\n"))
}
