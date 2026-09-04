// message.go is the mail-format layer, transport-independent: the two message
// shapes the bridge deals in, the `[forge #<task>]` subject marker that makes a
// plain reply an answer, RFC 5322 assembly for the SMTP path, and the parsing
// that turns a received message back into the text a human typed.
package main

import (
	"bytes"
	"crypto/rand"
	"encoding/base64"
	"fmt"
	"io"
	"mime"
	"mime/multipart"
	"mime/quotedprintable"
	"net/mail"
	"regexp"
	"strings"
	"time"
)

// outgoing is one message Forge sends. InReplyTo carries the Message-ID being
// answered so mail clients thread the conversation.
type outgoing struct {
	Subject   string
	Body      string
	InReplyTo string
}

// inboundMail is one message read from the mailbox. ID is the transport's own
// identifier (an IMAP UID, a Graph message id) and the dedupe key.
type inboundMail struct {
	ID        string
	From      string
	Subject   string
	Body      string
	MessageID string
	// Auto marks machine-generated mail (an auto-reply, a bounce, or Forge's
	// own notification coming back): never acted on, so two mailboxes cannot
	// talk each other into a loop.
	Auto bool
}

// pluginHeader marks Forge's own mail so a copy that lands back in the inbox is
// recognised and ignored.
const pluginHeader = "X-Forge-Plugin"

// maxSubjectText bounds how much of a question or reason rides in the subject.
const maxSubjectText = 60

// taskMarker is the subject tag that ties a thread to a task: a reply keeps it
// ("Re: [forge #3f69f96a] …"), which is how an answer finds its question with
// no state kept on either side.
var taskMarker = regexp.MustCompile(`(?i)\[forge #([0-9a-f]{4,32})\]`)

// markerFor renders the subject tag for a work id.
func markerFor(workID string) string { return "[forge #" + shortID(workID) + "]" }

// taskRefFromSubject returns the task id a subject refers to, or "".
func taskRefFromSubject(subject string) string {
	m := taskMarker.FindStringSubmatch(subject)
	if m == nil {
		return ""
	}
	return strings.ToLower(m[1])
}

// subjectLine builds "[forge #id] headline", trimming the headline to one line
// of sane length — the body carries the detail.
func subjectLine(workID, headline string) string {
	headline = strings.TrimSpace(strings.SplitN(headline, "\n", 2)[0])
	if len(headline) > maxSubjectText {
		headline = strings.TrimSpace(headline[:maxSubjectText]) + "…"
	}
	if workID == "" {
		return headline
	}
	return markerFor(workID) + " " + headline
}

// replySubject keeps a thread together without stacking Re: prefixes.
func replySubject(subject string) string {
	subject = strings.TrimSpace(subject)
	if subject == "" {
		return "Re: Forge"
	}
	if strings.HasPrefix(strings.ToLower(subject), "re:") {
		return subject
	}
	return "Re: " + subject
}

// newMessageID mints an RFC 5322 Message-ID for a sent message; the random half
// comes from crypto/rand so two Forge instances never collide.
func newMessageID(host string) (string, error) {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		return "", fmt.Errorf("generate message id: %w", err)
	}
	if host == "" {
		host = "forge.local"
	}
	return fmt.Sprintf("<%s@%s>", base64.RawURLEncoding.EncodeToString(b), host), nil
}

// buildMIME assembles a UTF-8 plain-text message. Quoted-printable keeps
// non-ASCII bodies intact on 7-bit paths, and the marker header lets the
// inbound half recognise Forge's own mail.
func buildMIME(from, to string, m outgoing, messageID string, now time.Time) ([]byte, error) {
	var buf bytes.Buffer
	write := func(format string, args ...any) {
		fmt.Fprintf(&buf, format, args...)
	}
	write("From: %s\r\n", from)
	write("To: %s\r\n", to)
	write("Subject: %s\r\n", mime.QEncoding.Encode("utf-8", m.Subject))
	write("Date: %s\r\n", now.Format(time.RFC1123Z))
	write("Message-ID: %s\r\n", messageID)
	if m.InReplyTo != "" {
		write("In-Reply-To: %s\r\n", m.InReplyTo)
		write("References: %s\r\n", m.InReplyTo)
	}
	write("%s: email\r\n", pluginHeader)
	write("Auto-Submitted: auto-generated\r\n")
	write("MIME-Version: 1.0\r\n")
	write("Content-Type: text/plain; charset=utf-8\r\n")
	write("Content-Transfer-Encoding: quoted-printable\r\n\r\n")
	qp := quotedprintable.NewWriter(&buf)
	if _, err := io.WriteString(qp, normalizeNewlines(m.Body)); err != nil {
		return nil, fmt.Errorf("encode message body: %w", err)
	}
	if err := qp.Close(); err != nil {
		return nil, fmt.Errorf("finish message body: %w", err)
	}
	return buf.Bytes(), nil
}

// normalizeNewlines makes the body CRLF, as SMTP requires.
func normalizeNewlines(s string) string {
	return strings.ReplaceAll(strings.ReplaceAll(s, "\r\n", "\n"), "\n", "\r\n")
}

// maxBodyBytes bounds how much of one message is read into memory; a command
// or an answer is a few lines, and a giant attachment is never one.
const maxBodyBytes = 1 << 20

// parseMessage turns a raw RFC 5322 message into the fields the bridge reads:
// the sender, the subject, the first text/plain part, and whether it is
// machine-generated.
func parseMessage(id string, raw []byte) (inboundMail, error) {
	msg, err := mail.ReadMessage(bytes.NewReader(raw))
	if err != nil {
		return inboundMail{}, fmt.Errorf("parse message %s: %w", id, err)
	}
	dec := new(mime.WordDecoder)
	subject, err := dec.DecodeHeader(msg.Header.Get("Subject"))
	if err != nil {
		subject = msg.Header.Get("Subject")
	}
	body, err := textPart(msg.Header.Get("Content-Type"), msg.Header.Get("Content-Transfer-Encoding"), msg.Body)
	if err != nil {
		return inboundMail{}, fmt.Errorf("read body of message %s: %w", id, err)
	}
	return inboundMail{
		ID:        id,
		From:      addressOf(msg.Header.Get("From")),
		Subject:   subject,
		Body:      body,
		MessageID: strings.TrimSpace(msg.Header.Get("Message-ID")),
		Auto:      isAuto(msg.Header),
	}, nil
}

// isAuto reports machine-generated mail: an auto-responder, a bulk sender, or
// Forge's own notification echoed back.
func isAuto(h mail.Header) bool {
	if h.Get(pluginHeader) != "" {
		return true
	}
	if a := strings.ToLower(h.Get("Auto-Submitted")); a != "" && a != "no" {
		return true
	}
	switch strings.ToLower(h.Get("Precedence")) {
	case "bulk", "list", "junk":
		return true
	}
	return h.Get("X-Auto-Response-Suppress") != "" || h.Get("List-Id") != ""
}

// addressOf reduces a From header to the bare address, falling back to the raw
// header when it does not parse.
func addressOf(header string) string {
	if a, err := mail.ParseAddress(strings.TrimSpace(header)); err == nil {
		return a.Address
	}
	return strings.TrimSpace(header)
}

// textPart returns the first text/plain content of a message body, walking one
// level of multipart (the shape every mail client produces) and decoding the
// transfer encoding.
func textPart(contentType, encoding string, body io.Reader) (string, error) {
	mediaType, params, err := mime.ParseMediaType(contentType)
	if contentType == "" || err != nil {
		mediaType, params = "text/plain", nil
	}
	if !strings.HasPrefix(mediaType, "multipart/") {
		return decodeBody(encoding, body)
	}
	mr := multipart.NewReader(body, params["boundary"])
	var firstErr error
	for {
		part, perr := mr.NextPart()
		if perr == io.EOF {
			break
		}
		if perr != nil {
			return "", fmt.Errorf("walk multipart body: %w", perr)
		}
		ct := part.Header.Get("Content-Type")
		if ct == "" || strings.HasPrefix(strings.ToLower(ct), "text/plain") {
			text, derr := decodeBody(part.Header.Get("Content-Transfer-Encoding"), part)
			if derr == nil {
				return text, nil
			}
			if firstErr == nil {
				firstErr = derr
			}
		}
		if strings.HasPrefix(strings.ToLower(ct), "multipart/") {
			// multipart/alternative inside multipart/mixed: recurse once more.
			if text, terr := textPart(ct, "", part); terr == nil && text != "" {
				return text, nil
			}
		}
	}
	return "", firstErr
}

// decodeBody reads a part, undoing base64 or quoted-printable.
func decodeBody(encoding string, r io.Reader) (string, error) {
	limited := io.LimitReader(r, maxBodyBytes)
	switch strings.ToLower(strings.TrimSpace(encoding)) {
	case "base64":
		r = base64.NewDecoder(base64.StdEncoding, newlineStripper{limited})
	case "quoted-printable":
		r = quotedprintable.NewReader(limited)
	default:
		r = limited
	}
	b, err := io.ReadAll(r)
	if err != nil {
		return "", fmt.Errorf("decode %q body: %w", encoding, err)
	}
	return string(b), nil
}

// newlineStripper drops the line breaks base64 mail bodies are wrapped at,
// which the standard decoder rejects.
type newlineStripper struct{ r io.Reader }

func (s newlineStripper) Read(p []byte) (int, error) {
	n, err := s.r.Read(p)
	if n > 0 {
		clean := p[:0]
		for _, c := range p[:n] {
			if c != '\r' && c != '\n' {
				clean = append(clean, c)
			}
		}
		n = len(clean)
	}
	return n, err
}

// Quoted-reply openers every mail client writes above the text it quotes.
var replyOpeners = []*regexp.Regexp{
	regexp.MustCompile(`(?i)^on .*wrote:\s*$`),
	regexp.MustCompile(`(?i)^-{2,}\s*original message\s*-{2,}\s*$`),
	regexp.MustCompile(`(?i)^_{5,}\s*$`),
	regexp.MustCompile(`(?i)^from:\s.*$`),
	regexp.MustCompile(`(?i)^sent from my \w+`),
}

// replyText is what the human actually typed: everything above the quoted
// original, with quote markers and the signature dropped. A reply that is
// nothing but quoted text yields "" and is treated as no answer at all.
func replyText(body string) string {
	lines := strings.Split(strings.ReplaceAll(body, "\r\n", "\n"), "\n")
	kept := make([]string, 0, len(lines))
	for _, line := range lines {
		trimmed := strings.TrimSpace(line)
		if trimmed == "--" || trimmed == "-- " {
			break // signature
		}
		if strings.HasPrefix(trimmed, ">") {
			continue
		}
		if isReplyOpener(trimmed) {
			break
		}
		kept = append(kept, line)
	}
	return strings.TrimSpace(strings.Join(kept, "\n"))
}

func isReplyOpener(line string) bool {
	for _, re := range replyOpeners {
		if re.MatchString(line) {
			return true
		}
	}
	return false
}

// sameAddress compares mail addresses the way a mailbox does: case-insensitive
// on the whole address, after reducing a display-name form to the address.
func sameAddress(a, b string) bool {
	a, b = strings.ToLower(addressOf(a)), strings.ToLower(addressOf(b))
	return a != "" && a == b
}
