package main

import (
	"bufio"
	"context"
	"fmt"
	"net"
	"strings"
	"testing"
)

// fakeIMAP scripts a server on one end of a pipe: it answers the five commands
// this client speaks, and serves the given raw messages by UID.
type fakeIMAP struct {
	messages map[string]string // uid -> raw RFC 5322 message
	uids     []string
	loginErr bool
	marked   []string // uids the client marked \Seen
}

func (f *fakeIMAP) serve(t *testing.T, conn net.Conn) {
	t.Helper()
	defer func() {
		if err := conn.Close(); err != nil {
			t.Logf("close server conn: %v", err)
		}
	}()
	w := bufio.NewWriter(conn)
	r := bufio.NewReader(conn)
	send := func(format string, args ...any) {
		fmt.Fprintf(w, format+"\r\n", args...)
		if err := w.Flush(); err != nil {
			t.Logf("flush: %v", err)
		}
	}
	send("* OK IMAP4rev1 ready")
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			return
		}
		fields := strings.Fields(strings.TrimRight(line, "\r\n"))
		if len(fields) < 2 {
			return
		}
		tag, cmd := fields[0], strings.ToUpper(fields[1])
		sub := ""
		if len(fields) > 2 {
			sub = strings.ToUpper(fields[2])
		}
		switch {
		case cmd == "LOGIN":
			if f.loginErr {
				send("%s NO [AUTHENTICATIONFAILED] Invalid credentials", tag)
				continue
			}
			send("%s OK LOGIN completed", tag)
		case cmd == "SELECT":
			send("* %d EXISTS", len(f.uids))
			send("%s OK [READ-WRITE] SELECT completed", tag)
		case cmd == "UID" && sub == "SEARCH":
			send("* SEARCH %s", strings.Join(f.uids, " "))
			send("%s OK SEARCH completed", tag)
		case cmd == "UID" && sub == "FETCH":
			uid := fields[3]
			raw, ok := f.messages[uid]
			if !ok {
				send("%s OK FETCH completed", tag) // vanished between SEARCH and FETCH
				continue
			}
			send("* 1 FETCH (UID %s BODY[] {%d}", uid, len(raw))
			if _, werr := w.WriteString(raw + ")\r\n"); werr != nil {
				t.Logf("write literal: %v", werr)
			}
			if ferr := w.Flush(); ferr != nil {
				t.Logf("flush literal: %v", ferr)
			}
			send("%s OK FETCH completed", tag)
		case cmd == "UID" && sub == "STORE":
			f.marked = append(f.marked, fields[3])
			send("%s OK STORE completed", tag)
		case cmd == "LOGOUT":
			send("* BYE")
			send("%s OK LOGOUT completed", tag)
			return
		default:
			send("%s BAD unknown command", tag)
		}
	}
}

func newTestReader(t *testing.T, srv *fakeIMAP) *imapReader {
	t.Helper()
	return &imapReader{
		cfg: imapConfig{Username: "forge@example.com", Password: "pw", Mailbox: "INBOX"},
		dial: func(context.Context) (net.Conn, error) {
			client, server := net.Pipe()
			go srv.serve(t, server)
			return client, nil
		},
	}
}

func TestIMAPFetchReadsLiteralsAndMarksSeen(t *testing.T) {
	body := "From: Nate <nate@example.com>\r\nSubject: Re: [forge #ab12] question\r\n" +
		"Message-ID: <r1@example.com>\r\n\r\nuse main\r\n"
	srv := &fakeIMAP{uids: []string{"101"}, messages: map[string]string{"101": body}}
	msgs, err := newTestReader(t, srv).fetch(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(msgs) != 1 {
		t.Fatalf("read %d messages, want 1", len(msgs))
	}
	m := msgs[0]
	if m.ID != "101" || m.From != "nate@example.com" || strings.TrimSpace(m.Body) != "use main" {
		t.Fatalf("message = %+v", m)
	}
	if len(srv.marked) != 1 || srv.marked[0] != "101" {
		t.Errorf("marked = %v, want the message read", srv.marked)
	}
}

// A message the parser chokes on is marked read and skipped, so it cannot wedge
// the mailbox forever.
func TestIMAPSkipsUnparseableMessage(t *testing.T) {
	srv := &fakeIMAP{uids: []string{"101", "102"}, messages: map[string]string{
		"101": "this is not a message at all\r\n",
		"102": "From: nate@example.com\r\nSubject: hi\r\n\r\nfile a task\r\n",
	}}
	msgs, err := newTestReader(t, srv).fetch(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(msgs) != 1 || msgs[0].ID != "102" {
		t.Fatalf("messages = %+v", msgs)
	}
	if len(srv.marked) != 2 {
		t.Errorf("both messages should be marked read, got %v", srv.marked)
	}
}

func TestIMAPLoginFailureIsReported(t *testing.T) {
	srv := &fakeIMAP{loginErr: true}
	_, err := newTestReader(t, srv).fetch(context.Background())
	if err == nil || !strings.Contains(err.Error(), "AUTHENTICATIONFAILED") {
		t.Fatalf("want the server's own words, got %v", err)
	}
	if strings.Contains(err.Error(), "pw") {
		t.Errorf("an error must never carry the password: %v", err)
	}
}

// A UID that vanishes between SEARCH and FETCH is skipped, not an error.
func TestIMAPToleratesVanishedMessage(t *testing.T) {
	srv := &fakeIMAP{uids: []string{"101"}, messages: map[string]string{}}
	msgs, err := newTestReader(t, srv).fetch(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(msgs) != 0 {
		t.Fatalf("messages = %+v", msgs)
	}
}

func TestIMAPFetchLimit(t *testing.T) {
	srv := &fakeIMAP{messages: map[string]string{}}
	for i := range fetchLimit + 5 {
		uid := fmt.Sprint(100 + i)
		srv.uids = append(srv.uids, uid)
		srv.messages[uid] = "From: nate@example.com\r\nSubject: hi\r\n\r\nbody\r\n"
	}
	msgs, err := newTestReader(t, srv).fetch(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(msgs) != fetchLimit {
		t.Fatalf("read %d messages, want the %d-message cap", len(msgs), fetchLimit)
	}
}

func TestLiteralSizeAndSearchParsing(t *testing.T) {
	if n, ok := literalSize("* 1 FETCH (UID 3 BODY[] {2345}"); !ok || n != 2345 {
		t.Errorf("literalSize = %d, %v", n, ok)
	}
	if _, ok := literalSize("* 1 FETCH (FLAGS (\\Seen))"); ok {
		t.Error("a line with no literal must not report one")
	}
	got := parseSearch([]string{"* 3 EXISTS", "* SEARCH 1 2 17", "* OK noise"})
	if strings.Join(got, ",") != "1,2,17" {
		t.Errorf("parseSearch = %v", got)
	}
	if len(parseSearch([]string{"* SEARCH"})) != 0 {
		t.Error("an empty SEARCH result must yield no uids")
	}
}

func TestQuoteIMAPEscapes(t *testing.T) {
	if got := quoteIMAP(`pa"ss\word`); got != `"pa\"ss\\word"` {
		t.Errorf("quoteIMAP = %s", got)
	}
}
