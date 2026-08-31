// gh.go is the GitHub side: the small view of an issue this plugin needs and
// the gitHub interface the poller drives, with a real implementation that
// shells out to the host's `gh` CLI. `gh` is never handed a token — it reaches
// the user's credential store through the XDG environment the daemon passes
// through, so the plugin only ever chooses the argv. Tests inject a fake
// gitHub and never exec anything.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
)

// issue is the slice of a GitHub issue this plugin reads (the fields asked of
// `gh issue list --json`).
type issue struct {
	Number int    `json:"number"`
	Title  string `json:"title"`
	Body   string `json:"body"`
	URL    string `json:"url"`
}

// gitHub is every GitHub operation the poller performs, so tests substitute a
// fake. The concrete implementation is execGitHub.
type gitHub interface {
	ListIssues(ctx context.Context, repo, label string) ([]issue, error)
	Comment(ctx context.Context, repo string, number int, body string) error
	Label(ctx context.Context, repo string, number int, label string) error
}

// execGitHub runs the host's `gh` CLI. It carries no state: authentication is
// the CLI's own, via the passed-through XDG environment.
type execGitHub struct{}

// ListIssues returns the open issues on repo, optionally filtered to label.
func (execGitHub) ListIssues(ctx context.Context, repo, label string) ([]issue, error) {
	args := []string{"issue", "list", "--repo", repo, "--state", "open", "--limit", "50", "--json", "number,title,body,url"}
	if label != "" {
		args = append(args, "--label", label)
	}
	out, err := runGH(ctx, args...)
	if err != nil {
		return nil, err
	}
	return decodeIssues(out)
}

// Comment posts body as a new comment on issue number.
func (execGitHub) Comment(ctx context.Context, repo string, number int, body string) error {
	_, err := runGH(ctx, "issue", "comment", strconv.Itoa(number), "--repo", repo, "--body", body)
	return err
}

// Label adds label to issue number. The caller treats a failure as a warning
// (a label absent from the repo is not fatal).
func (execGitHub) Label(ctx context.Context, repo string, number int, label string) error {
	_, err := runGH(ctx, "issue", "edit", strconv.Itoa(number), "--repo", repo, "--add-label", label)
	return err
}

// runGH executes `gh` with args, returning stdout. A non-zero exit carries the
// trimmed stderr so the log line says what gh complained about.
func runGH(ctx context.Context, args ...string) ([]byte, error) {
	cmd := exec.CommandContext(ctx, "gh", args...)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		msg := strings.TrimSpace(stderr.String())
		if msg == "" {
			msg = err.Error()
		}
		return nil, fmt.Errorf("gh %s: %s", strings.Join(args, " "), msg)
	}
	return stdout.Bytes(), nil
}

// decodeIssues parses `gh issue list --json` output, dropping any entry with a
// non-positive number — a malformed row that could not become a task anyway.
func decodeIssues(data []byte) ([]issue, error) {
	var raw []issue
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, fmt.Errorf("decode gh issue list: %w", err)
	}
	out := make([]issue, 0, len(raw))
	for _, is := range raw {
		if is.Number <= 0 {
			continue
		}
		out = append(out, is)
	}
	return out, nil
}
