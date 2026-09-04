// imap.go is the inbound boundary for mode "smtp": a deliberately small IMAP
// client that does one job — hand over the unread messages in a mailbox and
// mark them read. It speaks the five commands that job needs (LOGIN, SELECT,
// SEARCH, FETCH, STORE) and understands one piece of IMAP syntax beyond
// line-reading: the `{n}` literal. That is a fraction of RFC 3501, and it is
// the whole reason this plugin needs no third-party dependency.
//
// Providers that have disabled basic authentication (Microsoft 365 by default)
// cannot be read this way — use mode "graph" for those.
package main

import (
	"bufio"
	"context"
	"crypto/tls"
	"fmt"
	"io"
	"net"
	"strconv"
	"strings"
	"time"
)

const (
	// imapTimeout bounds one whole poll: connect, read, mark, log out.
	imapTimeout = 60 * time.Second
	// fetchLimit bounds how many unread messages one poll takes; the rest wait
	// for the next tick, so a flooded mailbox cannot stall the loop.
	fetchLimit = 20
	// literalLimit bounds one message read into memory.
	literalLimit = 4 << 20
)

// imapReader reads unread mail from one mailbox. dial is injected so tests can
// drive a scripted server over an in-memory pipe.
type imapReader struct {
	cfg  imapConfig
	dial func(ctx context.Context) (net.Conn, error)
}

func newIMAPReader(cfg imapConfig) *imapReader {
	return &imapReader{cfg: cfg, dial: func(ctx context.Context) (net.Conn, error) {
		addr := net.JoinHostPort(cfg.Host, strconv.Itoa(cfg.Port))
		conn, err := (&tls.Dialer{Config: &tls.Config{ServerName: cfg.Host, MinVersion: tls.VersionTLS12}}).DialContext(ctx, "tcp", addr)
		if err != nil {
			return nil, fmt.Errorf("dial %s over TLS: %w", addr, err)
		}
		return conn, nil
	}}
}

// fetch returns the mailbox's unread messages and marks them read, so the next
// poll does not see them again. Marking happens per message, right after it is
// read: a crash mid-poll re-reads at most the messages it had not yet marked,
// and the bridge's own dedupe catches those.
func (r *imapReader) fetch(ctx context.Context) (_ []inboundMail, err error) {
	cctx, cancel := context.WithTimeout(ctx, imapTimeout)
	defer cancel()
	conn, err := r.dial(cctx)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := conn.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close imap connection: %w", cerr)
		}
	}()
	if deadline, ok := cctx.Deadline(); ok {
		if derr := conn.SetDeadline(deadline); derr != nil {
			return nil, fmt.Errorf("set imap deadline: %w", derr)
		}
	}
	c := &imapConn{r: bufio.NewReader(conn), w: conn}
	if err := c.greeting(); err != nil {
		return nil, err
	}
	if _, err := c.exec("LOGIN " + quoteIMAP(r.cfg.Username) + " " + quoteIMAP(r.cfg.Password)); err != nil {
		return nil, fmt.Errorf("imap login as %s: %w", r.cfg.Username, err)
	}
	if _, err := c.exec("SELECT " + quoteIMAP(r.cfg.Mailbox)); err != nil {
		return nil, fmt.Errorf("select mailbox %s: %w", r.cfg.Mailbox, err)
	}
	lines, err := c.exec("UID SEARCH UNSEEN")
	if err != nil {
		return nil, fmt.Errorf("search unread mail: %w", err)
	}
	uids := parseSearch(lines)
	if len(uids) > fetchLimit {
		uids = uids[:fetchLimit]
	}
	var msgs []inboundMail
	for _, uid := range uids {
		raw, ferr := c.fetchBody(uid)
		if ferr != nil {
			return msgs, ferr
		}
		if raw == nil {
			continue // vanished between SEARCH and FETCH
		}
		m, perr := parseMessage(uid, raw)
		if perr != nil {
			// A message this plugin cannot parse must not block the mailbox:
			// mark it read and move on.
			if _, serr := c.exec("UID STORE " + uid + " +FLAGS (\\Seen)"); serr != nil {
				return msgs, serr
			}
			continue
		}
		if _, serr := c.exec("UID STORE " + uid + " +FLAGS (\\Seen)"); serr != nil {
			return msgs, fmt.Errorf("mark message %s read: %w", uid, serr)
		}
		msgs = append(msgs, m)
	}
	if _, err := c.exec("LOGOUT"); err != nil {
		return msgs, fmt.Errorf("imap logout: %w", err)
	}
	return msgs, nil
}

// imapConn is one connection's protocol state: the reader, the writer, and the
// command counter that makes tags unique.
type imapConn struct {
	r   *bufio.Reader
	w   io.Writer
	seq int
}

// greeting consumes the server's untagged hello.
func (c *imapConn) greeting() error {
	line, err := c.readLine()
	if err != nil {
		return fmt.Errorf("read imap greeting: %w", err)
	}
	if !strings.HasPrefix(line, "* OK") && !strings.HasPrefix(line, "* PREAUTH") {
		return fmt.Errorf("imap server refused the connection: %s", line)
	}
	return nil
}

// exec runs one command and returns the untagged lines it produced. A NO or BAD
// completion is an error carrying the server's own words.
func (c *imapConn) exec(cmd string) ([]string, error) {
	tag, err := c.write(cmd)
	if err != nil {
		return nil, err
	}
	var untagged []string
	for {
		line, rerr := c.readLine()
		if rerr != nil {
			return untagged, fmt.Errorf("read response to %s: %w", firstWord(cmd), rerr)
		}
		if strings.HasPrefix(line, tag+" ") {
			rest := strings.TrimSpace(strings.TrimPrefix(line, tag+" "))
			if strings.HasPrefix(rest, "OK") {
				return untagged, nil
			}
			return untagged, fmt.Errorf("%s", rest)
		}
		untagged = append(untagged, line)
	}
}

// fetchBody reads one message by UID, returning nil when the server reports no
// such message. It handles the one syntax that matters: a `{n}` literal, whose
// n bytes follow the line verbatim.
func (c *imapConn) fetchBody(uid string) ([]byte, error) {
	tag, err := c.write("UID FETCH " + uid + " (BODY.PEEK[])")
	if err != nil {
		return nil, err
	}
	var body []byte
	for {
		line, rerr := c.readLine()
		if rerr != nil {
			return nil, fmt.Errorf("read message %s: %w", uid, rerr)
		}
		if strings.HasPrefix(line, tag+" ") {
			rest := strings.TrimSpace(strings.TrimPrefix(line, tag+" "))
			if strings.HasPrefix(rest, "OK") {
				return body, nil
			}
			return nil, fmt.Errorf("fetch message %s: %s", uid, rest)
		}
		size, ok := literalSize(line)
		if !ok {
			continue
		}
		if size > literalLimit {
			return nil, fmt.Errorf("message %s is %d bytes, over the %d-byte limit", uid, size, literalLimit)
		}
		buf := make([]byte, size)
		if _, rerr := io.ReadFull(c.r, buf); rerr != nil {
			return nil, fmt.Errorf("read message %s body: %w", uid, rerr)
		}
		body = buf
	}
}

// write sends one tagged command and returns its tag.
func (c *imapConn) write(cmd string) (string, error) {
	c.seq++
	tag := fmt.Sprintf("a%03d", c.seq)
	if _, err := io.WriteString(c.w, tag+" "+cmd+"\r\n"); err != nil {
		return "", fmt.Errorf("send %s: %w", firstWord(cmd), err)
	}
	return tag, nil
}

// readLine reads one CRLF-terminated protocol line, without the terminator.
func (c *imapConn) readLine() (string, error) {
	line, err := c.r.ReadString('\n')
	if err != nil {
		return "", err
	}
	return strings.TrimRight(line, "\r\n"), nil
}

// literalSize reports the byte count of a trailing `{n}` literal marker.
func literalSize(line string) (int, bool) {
	i := strings.LastIndex(line, "{")
	if i < 0 || !strings.HasSuffix(line, "}") {
		return 0, false
	}
	n, err := strconv.Atoi(line[i+1 : len(line)-1])
	if err != nil || n < 0 {
		return 0, false
	}
	return n, true
}

// parseSearch pulls the UIDs out of an untagged `* SEARCH 1 2 3` response.
func parseSearch(lines []string) []string {
	var uids []string
	for _, line := range lines {
		fields := strings.Fields(line)
		if len(fields) < 2 || fields[0] != "*" || !strings.EqualFold(fields[1], "SEARCH") {
			continue
		}
		for _, f := range fields[2:] {
			if _, err := strconv.Atoi(f); err == nil {
				uids = append(uids, f)
			}
		}
	}
	return uids
}

// quoteIMAP renders an astring as a quoted string, escaping the two characters
// IMAP requires — enough for a mailbox name, a username, or a password.
func quoteIMAP(s string) string {
	r := strings.NewReplacer(`\`, `\\`, `"`, `\"`)
	return `"` + r.Replace(s) + `"`
}

// firstWord names the command in an error, without leaking its arguments (a
// LOGIN's argument is a password).
func firstWord(cmd string) string {
	if i := strings.IndexByte(cmd, ' '); i > 0 {
		return cmd[:i]
	}
	return cmd
}
