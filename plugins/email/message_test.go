package main

import (
	"strings"
	"testing"
	"time"
)

// The subject marker is the whole answer-routing mechanism: it must survive the
// round trip through a mail client's "Re:".
func TestSubjectMarkerRoundTrip(t *testing.T) {
	subject := subjectLine("3f69f96a1122334455", "Forge: inventory needs input")
	if !strings.HasPrefix(subject, "[forge #3f69f96a] ") {
		t.Fatalf("subject = %q", subject)
	}
	if got := taskRefFromSubject("Re: " + subject); got != "3f69f96a" {
		t.Errorf("ref from a reply = %q", got)
	}
	if got := taskRefFromSubject("RE: Fwd: " + strings.ToUpper(subject)); got != "3f69f96a" {
		t.Errorf("ref should be case-insensitive, got %q", got)
	}
	if got := taskRefFromSubject("just a normal email"); got != "" {
		t.Errorf("unmarked subject gave %q", got)
	}
}

func TestSubjectLineTrimsToOneLine(t *testing.T) {
	long := strings.Repeat("x", maxSubjectText+40)
	got := subjectLine("abc12345", "first line\nsecond line")
	if strings.Contains(got, "second") {
		t.Errorf("subject kept a second line: %q", got)
	}
	got = subjectLine("", long)
	if len([]rune(got)) > maxSubjectText+1 {
		t.Errorf("subject not trimmed: %q", got)
	}
	if !strings.HasSuffix(got, "…") {
		t.Errorf("a trimmed subject should say so: %q", got)
	}
}

func TestReplySubjectDoesNotStack(t *testing.T) {
	if got := replySubject("[forge #ab12] hi"); got != "Re: [forge #ab12] hi" {
		t.Errorf("got %q", got)
	}
	if got := replySubject("Re: already"); got != "Re: already" {
		t.Errorf("got %q", got)
	}
	if got := replySubject(""); got != "Re: Forge" {
		t.Errorf("got %q", got)
	}
}

func TestBuildMIMEHeadersAndBody(t *testing.T) {
	msg := outgoing{Subject: "[forge #ab12] café", Body: "line one\nline two", InReplyTo: "<abc@forge.local>"}
	raw, err := buildMIME("forge@example.com", "nate@example.com", msg, "<id@forge.local>", time.Now())
	if err != nil {
		t.Fatal(err)
	}
	got := string(raw)
	for _, want := range []string{
		"From: forge@example.com\r\n", "To: nate@example.com\r\n",
		"In-Reply-To: <abc@forge.local>\r\n", "References: <abc@forge.local>\r\n",
		"Message-ID: <id@forge.local>\r\n", pluginHeader + ": email\r\n",
		"Auto-Submitted: auto-generated\r\n", "Content-Transfer-Encoding: quoted-printable\r\n",
	} {
		if !strings.Contains(got, want) {
			t.Errorf("message is missing %q:\n%s", want, got)
		}
	}
	if strings.Contains(got, "Subject: [forge #ab12] café") {
		t.Error("a non-ASCII subject must be encoded, not sent raw")
	}
	// The body round-trips through the parser.
	parsed, err := parseMessage("1", raw)
	if err != nil {
		t.Fatal(err)
	}
	if parsed.Body != "line one\r\nline two" {
		t.Errorf("body = %q", parsed.Body)
	}
	if parsed.Subject != "[forge #ab12] café" {
		t.Errorf("subject decoded to %q", parsed.Subject)
	}
	if !parsed.Auto {
		t.Error("Forge's own mail must be recognised as machine-generated")
	}
}

func TestParseMessageMultipartAndEncodings(t *testing.T) {
	raw := "From: Nate <nate@example.com>\r\n" +
		"Subject: Re: [forge #ab12] question\r\n" +
		"Message-ID: <reply@example.com>\r\n" +
		"MIME-Version: 1.0\r\n" +
		"Content-Type: multipart/alternative; boundary=\"BOUND\"\r\n\r\n" +
		"--BOUND\r\n" +
		"Content-Type: text/plain; charset=utf-8\r\n" +
		"Content-Transfer-Encoding: base64\r\n\r\n" +
		"dXNlIG1haW4=\r\n" +
		"--BOUND\r\n" +
		"Content-Type: text/html; charset=utf-8\r\n\r\n" +
		"<p>use main</p>\r\n" +
		"--BOUND--\r\n"
	m, err := parseMessage("7", []byte(raw))
	if err != nil {
		t.Fatal(err)
	}
	if m.From != "nate@example.com" {
		t.Errorf("from = %q", m.From)
	}
	if m.Body != "use main" {
		t.Errorf("body = %q (the text/plain part, base64-decoded)", m.Body)
	}
	if m.Auto {
		t.Error("a human reply must not be flagged automatic")
	}
	if taskRefFromSubject(m.Subject) != "ab12" {
		t.Errorf("subject ref = %q", taskRefFromSubject(m.Subject))
	}
}

func TestIsAutoCatchesTheLoopSources(t *testing.T) {
	for _, header := range []string{
		"Auto-Submitted: auto-replied", "Precedence: bulk",
		"List-Id: <forge.example.com>", "X-Auto-Response-Suppress: All",
		pluginHeader + ": email",
	} {
		raw := "From: a@example.com\r\nSubject: s\r\n" + header + "\r\n\r\nbody\r\n"
		m, err := parseMessage("1", []byte(raw))
		if err != nil {
			t.Fatal(err)
		}
		if !m.Auto {
			t.Errorf("%q should mark the message automatic", header)
		}
	}
}

func TestReplyTextKeepsOnlyWhatTheHumanTyped(t *testing.T) {
	body := "use the main branch\n\nOn Tue, Sep 1, 2026 at 9:04 AM Forge <forge@example.com> wrote:\n> which branch?\n> http://127.0.0.1:7340/tasks/abc\n"
	if got := replyText(body); got != "use the main branch" {
		t.Errorf("got %q", got)
	}
	outlook := "yes, ship it\r\n\r\n-----Original Message-----\r\nFrom: Forge\r\nwhich branch?\r\n"
	if got := replyText(outlook); got != "yes, ship it" {
		t.Errorf("got %q", got)
	}
	sig := "do it\n--\nNate\nSent from my phone\n"
	if got := replyText(sig); got != "do it" {
		t.Errorf("got %q", got)
	}
	if got := replyText("> only quoted text\n"); got != "" {
		t.Errorf("a reply with nothing new must yield nothing, got %q", got)
	}
}

func TestSameAddressIgnoresDisplayNamesAndCase(t *testing.T) {
	if !sameAddress("Nate <Nate@Example.com>", "nate@example.com") {
		t.Error("display name and case must not matter")
	}
	if sameAddress("", "nate@example.com") || sameAddress("a@example.com", "b@example.com") {
		t.Error("different addresses must not match")
	}
}

func TestNewMessageIDIsUniqueAndWellFormed(t *testing.T) {
	a, err := newMessageID("example.com")
	if err != nil {
		t.Fatal(err)
	}
	b, err := newMessageID("example.com")
	if err != nil {
		t.Fatal(err)
	}
	if a == b {
		t.Error("two message ids must differ")
	}
	if !strings.HasPrefix(a, "<") || !strings.HasSuffix(a, "@example.com>") {
		t.Errorf("message id = %q", a)
	}
	id, err := newMessageID("")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasSuffix(id, "@forge.local>") {
		t.Errorf("no host should fall back to forge.local, got %q", id)
	}
}

func TestHostOf(t *testing.T) {
	if got := hostOf("Forge <forge@example.com>"); got != "example.com" {
		t.Errorf("got %q", got)
	}
	if got := hostOf("not-an-address"); got != "" {
		t.Errorf("got %q", got)
	}
}
