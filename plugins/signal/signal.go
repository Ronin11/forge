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
	"strings"
	"time"
)

// signalCLI runs signal-cli for one account.
type signalCLI struct {
	bin     string
	account string
}

// send delivers one message to a recipient. A failure is returned, never fatal —
// a missed message must not kill the plugin.
func (s signalCLI) send(ctx context.Context, recipient, message string) error {
	cctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	cmd := exec.CommandContext(cctx, s.bin, "-a", s.account, "send", "-m", message, recipient)
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("signal-cli send: %w: %s", err, strings.TrimSpace(stderr.String()))
	}
	return nil
}

// incoming is one received text message.
type incoming struct {
	From string
	Text string
}

// envelope is the slice of signal-cli's `-o json receive` output this plugin
// needs: the sender and the message body.
type envelope struct {
	Envelope struct {
		Source       string `json:"source"`
		SourceNumber string `json:"sourceNumber"`
		DataMessage  struct {
			Message string `json:"message"`
		} `json:"dataMessage"`
	} `json:"envelope"`
}

// receive drains queued messages, waiting up to timeout for new ones. It parses
// signal-cli's line-delimited JSON; non-text envelopes (receipts, typing) yield
// no incoming and are skipped.
func (s signalCLI) receive(ctx context.Context, timeout time.Duration) ([]incoming, error) {
	cctx, cancel := context.WithTimeout(ctx, timeout+10*time.Second)
	defer cancel()
	secs := int(timeout.Seconds())
	if secs < 1 {
		secs = 1
	}
	cmd := exec.CommandContext(cctx, s.bin, "-a", s.account, "-o", "json", "receive", "-t", fmt.Sprintf("%d", secs))
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
		msgs = append(msgs, incoming{From: from, Text: text})
	}
	return msgs, sc.Err()
}
