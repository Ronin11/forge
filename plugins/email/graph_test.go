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
)

func TestHTMLToTextRendersAReply(t *testing.T) {
	cases := map[string]string{
		"<p>use main</p>":                      "use main",
		"<div>one</div><div>two</div>":         "one\ntwo",
		"a &amp; b":                            "a & b",
		"<style>p{color:red}</style><p>hi</p>": "hi",
		"<p>&nbsp;spaced&nbsp;</p>":            "spaced",
	}
	for in, want := range cases {
		if got := htmlToText(in); got != want {
			t.Errorf("htmlToText(%q) = %q, want %q", in, got, want)
		}
	}
}

// send posts a Graph sendMail payload with the plugin header, and fetch reads
// unread mail, flags Forge's own messages as automatic, and marks each read.
func TestGraphSendAndFetch(t *testing.T) {
	var sent map[string]any
	patched := []string{}
	tokens := 0
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case strings.HasSuffix(r.URL.Path, "/oauth2/v2.0/token"):
			tokens++
			fmt.Fprint(w, `{"access_token":"tok","expires_in":3600}`)
		case strings.HasSuffix(r.URL.Path, "/sendMail"):
			body, err := io.ReadAll(r.Body)
			if err != nil {
				t.Errorf("read sendMail body: %v", err)
			}
			if err := json.Unmarshal(body, &sent); err != nil {
				t.Errorf("decode sendMail body: %v", err)
			}
			w.WriteHeader(http.StatusAccepted)
		case r.Method == http.MethodPatch:
			patched = append(patched, r.URL.Path)
			w.WriteHeader(http.StatusOK)
		default:
			if got := r.Header.Get("Authorization"); got != "Bearer tok" {
				t.Errorf("Authorization = %q", got)
			}
			fmt.Fprint(w, `{"value":[
				{"id":"m1","subject":"Re: [forge #ab12] q","internetMessageId":"<r1@example.com>",
				 "body":{"contentType":"html","content":"<p>use main</p>"},
				 "from":{"emailAddress":{"address":"nate@example.com"}}},
				{"id":"m2","subject":"Forge: target failed","internetMessageId":"<r2@example.com>",
				 "body":{"contentType":"text","content":"echo"},
				 "from":{"emailAddress":{"address":"forge@example.com"}},
				 "internetMessageHeaders":[{"name":"X-Forge-Plugin","value":"email"}]}]}`)
		}
	}))
	defer srv.Close()
	g := newGraphMail(graphConfig{TenantID: "t", ClientID: "c", ClientSecret: "s", Mailbox: "forge@example.com"}, "nate@example.com", time.Now)
	g.base, g.login = srv.URL, srv.URL

	if err := g.send(context.Background(), outgoing{Subject: "[forge #ab12] hi", Body: "text"}); err != nil {
		t.Fatal(err)
	}
	msg, ok := sent["message"].(map[string]any)
	if !ok || msg["subject"] != "[forge #ab12] hi" {
		t.Fatalf("sendMail payload = %v", sent)
	}
	headers, ok := msg["internetMessageHeaders"].([]any)
	if !ok || len(headers) != 1 {
		t.Errorf("the plugin header must ride along: %v", msg["internetMessageHeaders"])
	}

	msgs, err := g.fetch(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(msgs) != 2 {
		t.Fatalf("read %d messages, want 2", len(msgs))
	}
	if msgs[0].Body != "use main" || msgs[0].From != "nate@example.com" || msgs[0].Auto {
		t.Errorf("human message = %+v", msgs[0])
	}
	if !msgs[1].Auto {
		t.Errorf("Forge's own mail must be flagged automatic: %+v", msgs[1])
	}
	if len(patched) != 2 {
		t.Errorf("both messages should be marked read, got %v", patched)
	}
	if tokens != 1 {
		t.Errorf("minted %d tokens, want 1 (cached)", tokens)
	}
}

func TestGraphSurfacesErrors(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/oauth2/v2.0/token") {
			fmt.Fprint(w, `{"access_token":"tok","expires_in":3600}`)
			return
		}
		http.Error(w, `{"error":{"code":"ErrorAccessDenied"}}`, http.StatusForbidden)
	}))
	defer srv.Close()
	g := newGraphMail(graphConfig{TenantID: "t", ClientID: "c", ClientSecret: "s", Mailbox: "forge@example.com"}, "nate@example.com", time.Now)
	g.base, g.login = srv.URL, srv.URL
	if _, err := g.fetch(context.Background()); err == nil || !strings.Contains(err.Error(), "403") {
		t.Fatalf("want a 403 error, got %v", err)
	}
	if err := g.send(context.Background(), outgoing{Subject: "s"}); err == nil || !strings.Contains(err.Error(), "ErrorAccessDenied") {
		t.Fatalf("want the server's own words, got %v", err)
	}
}
