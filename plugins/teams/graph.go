// graph.go is the inbound boundary: Microsoft Graph, app-only. It exchanges the
// Entra app registration for a token (client credentials, cached until shortly
// before expiry) and reads a channel's messages and their thread replies, so a
// human can talk back to Forge in the same channel Forge posts to. Everything
// is plain HTTPS + JSON — no SDK.
//
// App-only channel-message reads need ChannelMessage.Read.All *and* Microsoft's
// protected-API approval for the tenant (see README); until that is granted the
// poll gets 403s, which are logged, not fatal — the outbound half keeps working.
package main

import (
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
	// messagePage is how many top-level channel messages one poll reads; each
	// carries its replies, so a busy channel still fits in one request.
	messagePage = 20
	// graphTimeout bounds one Graph call.
	graphTimeout = 30 * time.Second
)

// incoming is one channel message a human posted: enough to dedupe it, decide
// whether to honor it, and act on its text.
type incoming struct {
	ID      string
	From    string // display name, UPN, or object id — whatever Graph gave
	Text    string
	Created time.Time
}

// graphClient reads one channel with one app registration. The mutex guards the
// cached token and its expiry, the only shared state.
type graphClient struct {
	cfg graphConfig
	hc  *http.Client
	now func() time.Time
	// Endpoints, injected so tests can point them at a local server; production
	// always uses graphBase and loginBase.
	graph string
	login string

	mu      sync.Mutex
	token   string
	expires time.Time
}

func newGraphClient(cfg graphConfig, now func() time.Time) *graphClient {
	return &graphClient{cfg: cfg, hc: &http.Client{Timeout: graphTimeout}, now: now, graph: graphBase, login: loginBase}
}

// tokenResponse is the slice of the OAuth2 token response used here.
type tokenResponse struct {
	AccessToken string `json:"access_token"`
	ExpiresIn   int64  `json:"expires_in"`
}

// accessToken returns a valid bearer token, minting a new one when the cached
// one is missing or close to expiry.
func (g *graphClient) accessToken(ctx context.Context) (string, error) {
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
	var out tokenResponse
	if err := g.request(ctx, http.MethodPost, endpoint, "", strings.NewReader(form.Encode()), &out); err != nil {
		return "", fmt.Errorf("mint graph token: %w", err)
	}
	if out.AccessToken == "" {
		return "", fmt.Errorf("mint graph token: response carried no access_token")
	}
	g.token, g.expires = out.AccessToken, g.now().Add(time.Duration(out.ExpiresIn)*time.Second)
	return g.token, nil
}

// messageList is the slice of a Graph channel-messages response this plugin
// reads — its own copy of the shape, like every other view type here.
type messageList struct {
	Value []graphMessage `json:"value"`
}

type graphMessage struct {
	ID              string    `json:"id"`
	MessageType     string    `json:"messageType"`
	CreatedDateTime time.Time `json:"createdDateTime"`
	DeletedDateTime *string   `json:"deletedDateTime"`
	Body            struct {
		ContentType string `json:"contentType"`
		Content     string `json:"content"`
	} `json:"body"`
	From struct {
		User *struct {
			ID                string `json:"id"`
			DisplayName       string `json:"displayName"`
			UserPrincipalName string `json:"userPrincipalName"`
		} `json:"user"`
	} `json:"from"`
	Replies []graphMessage `json:"replies"`
}

// messages returns human messages in the channel created after `after`, oldest
// first — top-level posts and thread replies alike, since a human answering a
// Forge card naturally replies in its thread.
func (g *graphClient) messages(ctx context.Context, after time.Time) ([]incoming, error) {
	token, err := g.accessToken(ctx)
	if err != nil {
		return nil, err
	}
	endpoint := fmt.Sprintf("%s/teams/%s/channels/%s/messages?$top=%d&$expand=replies",
		g.graph, url.PathEscape(g.cfg.TeamID), url.PathEscape(g.cfg.ChannelID), messagePage)
	var out messageList
	if err := g.request(ctx, http.MethodGet, endpoint, token, nil, &out); err != nil {
		return nil, fmt.Errorf("read channel messages: %w", err)
	}
	var msgs []incoming
	for _, m := range out.Value {
		msgs = appendHuman(msgs, m, after)
		for _, r := range m.Replies {
			msgs = appendHuman(msgs, r, after)
		}
	}
	sortByCreated(msgs)
	return msgs, nil
}

// appendHuman keeps only what this plugin can act on: an undeleted text message
// from a real user (Forge's own webhook posts arrive as an application, with no
// `from.user`, so they are skipped and cannot echo), newer than the cursor.
func appendHuman(dst []incoming, m graphMessage, after time.Time) []incoming {
	if m.DeletedDateTime != nil || m.From.User == nil {
		return dst
	}
	if m.MessageType != "" && m.MessageType != "message" {
		return dst
	}
	if !m.CreatedDateTime.After(after) {
		return dst
	}
	text := plainText(m.Body.Content)
	if text == "" {
		return dst
	}
	from := m.From.User.UserPrincipalName
	if from == "" {
		from = m.From.User.DisplayName
	}
	if from == "" {
		from = m.From.User.ID
	}
	return append(dst, incoming{ID: m.ID, From: from, Text: text, Created: m.CreatedDateTime})
}

// sortByCreated puts the oldest message first so commands are honored in the
// order they were typed. Insertion sort: a page is at most a few dozen rows.
func sortByCreated(msgs []incoming) {
	for i := 1; i < len(msgs); i++ {
		for j := i; j > 0 && msgs[j].Created.Before(msgs[j-1].Created); j-- {
			msgs[j], msgs[j-1] = msgs[j-1], msgs[j]
		}
	}
}

// allowed reports whether a sender's messages are honored: everyone when no
// allow-list is configured, else a case-insensitive match on UPN, display name,
// or object id.
func (g *graphClient) allowed(from string) bool {
	if len(g.cfg.AllowedUsers) == 0 {
		return true
	}
	for _, u := range g.cfg.AllowedUsers {
		if strings.EqualFold(strings.TrimSpace(u), strings.TrimSpace(from)) {
			return true
		}
	}
	return false
}

// request performs one JSON call, decoding into out. token empty means the call
// carries no Authorization header (the token endpoint itself).
func (g *graphClient) request(ctx context.Context, method, endpoint, token string, body io.Reader, out any) (err error) {
	cctx, cancel := context.WithTimeout(ctx, graphTimeout)
	defer cancel()
	req, err := http.NewRequestWithContext(cctx, method, endpoint, body)
	if err != nil {
		return fmt.Errorf("build %s %s: %w", method, endpoint, err)
	}
	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	}
	req.Header.Set("Accept", "application/json")
	resp, err := g.hc.Do(req)
	if err != nil {
		return fmt.Errorf("%s %s: %w", method, endpoint, err)
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close %s response: %w", endpoint, cerr)
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
			return fmt.Errorf("drain %s response: %w", endpoint, derr)
		}
		return nil
	}
	if derr := json.NewDecoder(resp.Body).Decode(out); derr != nil {
		return fmt.Errorf("decode %s response: %w", endpoint, derr)
	}
	return nil
}

// Teams message bodies are HTML. These are the three edits that matter for a
// command line: mentions (`<at>Forge</at>`) vanish, block ends become newlines,
// every other tag is dropped.
var (
	mentionTag = regexp.MustCompile(`(?is)<at\b[^>]*>.*?</at>`)
	breakTag   = regexp.MustCompile(`(?i)<(br\s*/?|/p|/div|/li)>`)
	anyTag     = regexp.MustCompile(`(?s)<[^>]*>`)
)

// plainText renders a Teams HTML body as the text a human typed. It is
// deliberately small: Forge only ever reads short command lines out of it.
func plainText(body string) string {
	s := mentionTag.ReplaceAllString(body, " ")
	s = breakTag.ReplaceAllString(s, "\n")
	s = anyTag.ReplaceAllString(s, "")
	s = html.UnescapeString(s)
	s = strings.ReplaceAll(s, "\u00a0", " ") // non-breaking spaces Teams inserts
	lines := strings.Split(s, "\n")
	kept := lines[:0]
	for _, ln := range lines {
		kept = append(kept, strings.TrimSpace(ln))
	}
	return strings.TrimSpace(strings.Join(kept, "\n"))
}
