package main

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestPlainTextRendersWhatAHumanTyped(t *testing.T) {
	cases := map[string]string{
		`<p>fix the <b>login</b> bug</p>`:                        "fix the login bug",
		`<div><at id="0">Forge</at> /answer ab12 use main</div>`: "/answer ab12 use main",
		"<p>one</p><p>two</p>":                                   "one\ntwo",
		`a &amp; b &lt;c&gt;`:                                    "a & b <c>",
		"<p>&nbsp;spaced&nbsp;</p>":                              "spaced",
		"":                                                       "",
	}
	for in, want := range cases {
		if got := plainText(in); got != want {
			t.Errorf("plainText(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestAppendHumanFiltersNonHumanMessages(t *testing.T) {
	after := time.Now().Add(-time.Hour)
	newer := after.Add(time.Minute)
	user := func(m *graphMessage) {
		m.From.User = &struct {
			ID                string `json:"id"`
			DisplayName       string `json:"displayName"`
			UserPrincipalName string `json:"userPrincipalName"`
		}{ID: "id-1", DisplayName: "Nate", UserPrincipalName: "nate@example.com"}
	}
	human := func() graphMessage {
		m := graphMessage{ID: "m1", MessageType: "message", CreatedDateTime: newer}
		m.Body.Content = "<p>hello</p>"
		user(&m)
		return m
	}
	if got := appendHuman(nil, human(), after); len(got) != 1 || got[0].From != "nate@example.com" || got[0].Text != "hello" {
		t.Fatalf("a human message should pass: %+v", got)
	}
	// Forge's own webhook cards arrive with no from.user — they must never echo.
	app := human()
	app.From.User = nil
	if got := appendHuman(nil, app, after); got != nil {
		t.Errorf("an application post must be skipped: %+v", got)
	}
	old := human()
	old.CreatedDateTime = after.Add(-time.Minute)
	if got := appendHuman(nil, old, after); got != nil {
		t.Errorf("a message at or before the cursor must be skipped: %+v", got)
	}
	deleted := human()
	when := "2026-01-01T00:00:00Z"
	deleted.DeletedDateTime = &when
	if got := appendHuman(nil, deleted, after); got != nil {
		t.Errorf("a deleted message must be skipped: %+v", got)
	}
	system := human()
	system.MessageType = "systemEventMessage"
	if got := appendHuman(nil, system, after); got != nil {
		t.Errorf("a system event must be skipped: %+v", got)
	}
	empty := human()
	empty.Body.Content = "<div><img src='x'></div>"
	if got := appendHuman(nil, empty, after); got != nil {
		t.Errorf("a message with no text must be skipped: %+v", got)
	}
}

func TestAllowedUsersMatchLoosely(t *testing.T) {
	open := &graphClient{}
	if !open.allowed("anyone@example.com") {
		t.Error("no allow-list means anyone in the channel")
	}
	g := &graphClient{cfg: graphConfig{AllowedUsers: []string{"Nate@Example.com", "id-1"}}}
	for _, from := range []string{"nate@example.com", "id-1"} {
		if !g.allowed(from) {
			t.Errorf("%q should be allowed", from)
		}
	}
	if g.allowed("stranger@example.com") {
		t.Error("an unlisted sender must be refused")
	}
}

// messages flattens top-level posts and thread replies, oldest first, and the
// token is minted once and reused until it nears expiry.
func TestMessagesReadsRepliesAndCachesToken(t *testing.T) {
	tokens := 0
	base := time.Date(2026, 9, 1, 12, 0, 0, 0, time.UTC)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/oauth2/v2.0/token") {
			tokens++
			fmt.Fprint(w, `{"access_token":"tok","expires_in":3600}`)
			return
		}
		if got := r.Header.Get("Authorization"); got != "Bearer tok" {
			t.Errorf("Authorization = %q", got)
		}
		fmt.Fprintf(w, `{"value":[{"id":"m2","messageType":"message","createdDateTime":%q,
			"body":{"content":"<p>second</p>"},"from":{"user":{"displayName":"Nate"}},
			"replies":[{"id":"m1","messageType":"message","createdDateTime":%q,
			"body":{"content":"<p>first</p>"},"from":{"user":{"displayName":"Nate"}}}]}]}`,
			base.Add(2*time.Minute).Format(time.RFC3339), base.Add(time.Minute).Format(time.RFC3339))
	}))
	defer srv.Close()
	g := newGraphClient(graphConfig{TenantID: "t", ClientID: "c", ClientSecret: "s", TeamID: "team", ChannelID: "chan"}, func() time.Time { return base })
	g.graph, g.login = srv.URL, srv.URL
	for range 2 {
		msgs, err := g.messages(context.Background(), base)
		if err != nil {
			t.Fatal(err)
		}
		if len(msgs) != 2 {
			t.Fatalf("read %d messages, want 2 (the post and its reply)", len(msgs))
		}
		if msgs[0].Text != "first" || msgs[1].Text != "second" {
			t.Fatalf("wrong order: %+v", msgs)
		}
	}
	if tokens != 1 {
		t.Errorf("minted %d tokens, want 1 (cached)", tokens)
	}
}

// A Graph failure is an error the caller logs, not a panic or silent success.
func TestMessagesSurfacesGraphErrors(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/oauth2/v2.0/token") {
			fmt.Fprint(w, `{"access_token":"tok","expires_in":3600}`)
			return
		}
		http.Error(w, `{"error":{"code":"Forbidden"}}`, http.StatusForbidden)
	}))
	defer srv.Close()
	g := newGraphClient(graphConfig{TenantID: "t", ClientID: "c", ClientSecret: "s", TeamID: "team", ChannelID: "chan"}, time.Now)
	g.graph, g.login = srv.URL, srv.URL
	_, err := g.messages(context.Background(), time.Time{})
	if err == nil || !strings.Contains(err.Error(), "403") {
		t.Fatalf("want a 403 error, got %v", err)
	}
}
