// signal.go is the signal-cli boundary: sending a message and draining received
// ones. signal-cli must be installed and its account registered/linked
// out-of-band (see plugin.toml). Everything is shelling out — no library, no
// long-lived daemon — matching the notify plugin's fire-and-forget shape.
package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"time"
)

// signalCLI runs signal-cli for one account. signal-cli holds an exclusive lock
// on the account store, so send and receive must never run at once — mu
// serializes every invocation.
type signalCLI struct {
	bin     string
	account string
	dir     string // a stable working directory: signal-cli (GraalVM native) fails
	// with "could not determine current working directory" if it inherits a CWD
	// that has been unlinked (e.g. the plugin dir swapped out by a reinstall).
	mu *sync.Mutex
}

// send delivers one message to a recipient. A failure is returned, never fatal —
// a missed message must not kill the plugin.
func (s signalCLI) send(ctx context.Context, recipient, message string) (int64, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	cctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	cmd := exec.CommandContext(cctx, s.bin, "-a", s.account, "send", "-m", message, recipient)
	cmd.Dir = s.dir
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		return 0, fmt.Errorf("signal-cli send: %w: %s", err, strings.TrimSpace(stderr.String()))
	}
	return parseSendTimestamp(stdout.String()), nil
}

// parseSendTimestamp pulls the sent-message id (a millisecond timestamp) that
// signal-cli prints on stdout; 0 if none is found (send still succeeded).
func parseSendTimestamp(out string) int64 {
	for _, f := range strings.Fields(out) {
		if n, err := strconv.ParseInt(f, 10, 64); err == nil && n > 1_000_000_000_000 {
			return n
		}
	}
	return 0
}

// incoming is one received text message.
type incoming struct {
	From    string
	Text    string
	QuoteID int64 // the timestamp of the message this one replies to (0 if not a reply)
}

// envelope is the slice of signal-cli's `-o json receive` output this plugin
// needs: the sender and the message body.
type envelope struct {
	Envelope struct {
		Source       string `json:"source"`
		SourceNumber string `json:"sourceNumber"`
		DataMessage  struct {
			Message string `json:"message"`
			Quote   struct {
				ID int64 `json:"id"`
			} `json:"quote"`
		} `json:"dataMessage"`
	} `json:"envelope"`
}

// receive drains queued messages, waiting up to timeout for new ones. It parses
// signal-cli's line-delimited JSON; non-text envelopes (receipts, typing) yield
// no incoming and are skipped.
func (s signalCLI) receive(ctx context.Context, timeout time.Duration) ([]incoming, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	// Cap the blocking wait: while receive holds the account lock a queued send
	// waits behind it, so keep the window short (the inbound loop just polls again).
	if timeout > 3*time.Second {
		timeout = 3 * time.Second
	}
	cctx, cancel := context.WithTimeout(ctx, timeout+10*time.Second)
	defer cancel()
	secs := int(timeout.Seconds())
	if secs < 1 {
		secs = 1
	}
	cmd := exec.CommandContext(cctx, s.bin, "-a", s.account, "-o", "json", "receive", "-t", fmt.Sprintf("%d", secs))
	cmd.Dir = s.dir
	var out, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &stderr
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("signal-cli receive: %w: %s", err, strings.TrimSpace(stderr.String()))
	}
	var msgs []incoming
	sc := bufio.NewScanner(&out)
	sc.Buffer(make([]byte, 64<<10), 4<<20)
	for sc.Scan() {
		line := bytes.TrimSpace(sc.Bytes())
		if len(line) == 0 {
			continue
		}
		var e envelope
		if json.Unmarshal(line, &e) != nil {
			continue
		}
		text := strings.TrimSpace(e.Envelope.DataMessage.Message)
		if text == "" {
			continue
		}
		from := e.Envelope.Source
		if from == "" {
			from = e.Envelope.SourceNumber
		}
		msgs = append(msgs, incoming{From: from, Text: text, QuoteID: e.Envelope.DataMessage.Quote.ID})
	}
	return msgs, sc.Err()
}
