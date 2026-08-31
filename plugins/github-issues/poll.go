// poll.go is the plugin's brain: one loop that turns new open issues into
// Forge tasks (ingest) and comments the outcome of finished tasks back on
// their issues (reconcile). It is decoupled from the daemon's Go packages —
// the terminal work states are redefined here as local constants rather than
// imported from internal/model — and pure with respect to its injected
// forgeAPI, gitHub, and clock, so tests never exec `gh` or a real daemon.
package main

import (
	"context"
	"fmt"
	"log/slog"
	"time"
)

// forgeAPI is every Forge operation the poller performs, so tests substitute a
// fake. The concrete implementation is *client.
type forgeAPI interface {
	CreateTask(ctx context.Context, req createTaskRequest) (string, error)
	Annotate(ctx context.Context, workID string, refs []externalRef) error
	TaskState(ctx context.Context, workID string) (*taskStatus, error)
}

// terminalStates are the WorkState strings that mean a task is finished
// (STYLE §2: this set has exactly one home in the plugin). Redefined locally
// rather than imported from internal/model, to stay decoupled like the other
// plugins.
var terminalStates = map[string]bool{
	"succeeded":  true,
	"failed":     true,
	"unverified": true,
	"partial":    true,
	"cancelled":  true,
	"merged":     true,
	"conflict":   true,
}

// titleMax bounds the task title's copy of the issue title.
const titleMax = 60

// poller owns the loop and the tracking state. One goroutine runs it, so the
// state needs no lock.
type poller struct {
	api forgeAPI
	gh  gitHub
	st  *state
	cfg config
	log *slog.Logger
	now func() time.Time
	ui  string
}

// run polls once immediately, then every poll_seconds, until the context is
// cancelled. With no repos configured it only heartbeats — never crashes.
func (p *poller) run(ctx context.Context) error {
	if len(p.cfg.Repos) == 0 {
		p.log.Info("no github-issues.toml; nothing to poll")
	} else {
		p.log.Info("github-issues starting", "repos", len(p.cfg.Repos), "poll_seconds", p.cfg.PollSeconds)
	}
	p.tick(ctx)
	t := time.NewTicker(time.Duration(p.cfg.PollSeconds) * time.Second)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-t.C:
			p.tick(ctx)
		}
	}
}

// tick runs one full cycle: ingest new issues, then reconcile finished tasks.
// A cancelled context ends the cycle promptly.
func (p *poller) tick(ctx context.Context) {
	if ctx.Err() != nil {
		return
	}
	if len(p.cfg.Repos) == 0 {
		p.log.Debug("heartbeat; nothing to poll")
		return
	}
	p.ingest(ctx)
	p.reconcile(ctx)
}

// ingest creates a task for every open issue not already tracked. One issue's
// failure is logged and skipped — the cycle never aborts, and an issue whose
// task was not created stays untracked so the next cycle retries it.
func (p *poller) ingest(ctx context.Context) {
	for _, repo := range p.cfg.Repos {
		if ctx.Err() != nil {
			return
		}
		issues, err := p.gh.ListIssues(ctx, repo.GitHub, repo.Label)
		if err != nil {
			p.log.Warn("list issues", "github", repo.GitHub, "err", err)
			continue
		}
		for _, is := range issues {
			if ctx.Err() != nil {
				return
			}
			key := issueKey(repo.GitHub, is.Number)
			if p.st.has(key) {
				continue
			}
			p.ingestOne(ctx, repo, is, key)
		}
	}
}

// ingestOne creates the task for one issue and, only once that succeeds,
// records the tracking entry — so an annotate or comment failure (best-effort)
// never causes a duplicate task on the next cycle. A create failure leaves the
// issue untracked, to retry.
func (p *poller) ingestOne(ctx context.Context, repo repoConfig, is issue, key string) {
	req := createTaskRequest{
		Prompt:       taskPrompt(is),
		Repositories: []string{repo.Forge},
		Mode:         repo.Mode,
		Class:        repo.Class,
		Autonomy:     repo.Autonomy,
		Integrate:    repo.Integrate,
		Title:        fmt.Sprintf("issue #%d: %s", is.Number, truncate(is.Title, titleMax)),
	}
	workID, err := p.api.CreateTask(ctx, req)
	if err != nil {
		p.log.Warn("create task", "github", repo.GitHub, "issue", is.Number, "err", err)
		return
	}
	p.log.Info("task created for issue", "github", repo.GitHub, "issue", is.Number, "work", shortID(workID))

	refs := []externalRef{{Kind: "issue", ID: key, URL: is.URL, Label: fmt.Sprintf("#%d", is.Number)}}
	if err := p.api.Annotate(ctx, workID, refs); err != nil {
		p.log.Warn("annotate task with issue ref", "work", shortID(workID), "err", err)
	}
	if repo.Comment {
		body := fmt.Sprintf("🔨 Forge picked this up as task `%s`. I'll comment when it's done.", shortID(workID))
		if err := p.gh.Comment(ctx, repo.GitHub, is.Number, body); err != nil {
			p.log.Warn("comment pickup", "github", repo.GitHub, "issue", is.Number, "err", err)
		}
	}

	entry := tracked{WorkID: workID, Forge: repo.Forge, Number: is.Number, GitHub: repo.GitHub, Status: statusIngested}
	if err := p.st.put(key, entry); err != nil {
		p.log.Error("persist state after ingest", "github", repo.GitHub, "issue", is.Number, "err", err)
	}
}

// reconcile checks each ingested task; when one reaches a terminal state it
// comments the outcome on the issue, best-effort labels it done, and marks the
// entry done so a later cycle does not comment again.
func (p *poller) reconcile(ctx context.Context) {
	// A stable snapshot of keys, since put mutates the map underneath.
	keys := make([]string, 0, len(p.st.entries))
	for k, e := range p.st.entries {
		if e.Status == statusIngested {
			keys = append(keys, k)
		}
	}
	for _, key := range keys {
		if ctx.Err() != nil {
			return
		}
		p.reconcileOne(ctx, key)
	}
}

// reconcileOne polls one tracked task and, if terminal, finishes it.
func (p *poller) reconcileOne(ctx context.Context, key string) {
	entry := p.st.entries[key]
	repo, ok := p.repoFor(entry.GitHub)
	if !ok {
		// The repo was removed from config; leave the entry untouched.
		return
	}
	status, err := p.api.TaskState(ctx, entry.WorkID)
	if err != nil {
		p.log.Warn("read task state", "work", shortID(entry.WorkID), "err", err)
		return
	}
	if !terminalStates[status.State] {
		return
	}
	body := outcomeComment(status, p.ui, entry.WorkID)
	if repo.Comment {
		if err := p.gh.Comment(ctx, entry.GitHub, entry.Number, body); err != nil {
			p.log.Warn("comment outcome", "github", entry.GitHub, "issue", entry.Number, "err", err)
			return // retry next cycle; the entry stays "ingested"
		}
	}
	if repo.DoneLabel != "" {
		if err := p.gh.Label(ctx, entry.GitHub, entry.Number, repo.DoneLabel); err != nil {
			p.log.Warn("add done label", "github", entry.GitHub, "issue", entry.Number, "label", repo.DoneLabel, "err", err)
		}
	}
	entry.Status = statusDone
	entry.DoneCommented = true
	if err := p.st.put(key, entry); err != nil {
		p.log.Error("persist state after outcome", "github", entry.GitHub, "issue", entry.Number, "err", err)
	}
	p.log.Info("commented outcome", "github", entry.GitHub, "issue", entry.Number, "state", status.State)
}

// repoFor finds the configured repo for a GitHub slug.
func (p *poller) repoFor(github string) (repoConfig, bool) {
	for _, r := range p.cfg.Repos {
		if r.GitHub == github {
			return r, true
		}
	}
	return repoConfig{}, false
}

// taskPrompt is the implement-mode prompt built from an issue; the issue is the
// acceptance criteria.
func taskPrompt(is issue) string {
	return fmt.Sprintf("GitHub issue #%d — %s\n\n%s\n\n(from %s)\nImplement a fix; the issue is the acceptance criteria.",
		is.Number, is.Title, is.Body, is.URL)
}

// outcomeComment renders the issue comment for a terminal task, keyed on its
// state, ending with a link to the task page.
func outcomeComment(status *taskStatus, ui, workID string) string {
	branch := pickBranch(status)
	var body string
	switch status.State {
	case "succeeded", "merged":
		body = fmt.Sprintf("✅ Done — %s on branch `%s`.", status.State, branch)
		if s := resultSummary(status); s != "" {
			body += " " + s
		}
	case "unverified":
		body = fmt.Sprintf("⚠️ I made a change on branch `%s` but couldn't verify it (%s). Needs a human look.", branch, status.State)
	default: // failed, partial, cancelled, conflict
		body = fmt.Sprintf("❌ Couldn't complete it (%s).", status.State)
	}
	return body + "\n" + ui + "/tasks/" + workID
}

// pickBranch returns the fix branch: the first target's branch, or the last
// attempt's — whichever is populated.
func pickBranch(status *taskStatus) string {
	if len(status.Targets) > 0 && status.Targets[0].Branch != "" {
		return status.Targets[0].Branch
	}
	for i := len(status.Attempts) - 1; i >= 0; i-- {
		if status.Attempts[i].Branch != "" {
			return status.Attempts[i].Branch
		}
	}
	return "unknown"
}

// resultSummary returns the last attempt's result summary, if any.
func resultSummary(status *taskStatus) string {
	for i := len(status.Attempts) - 1; i >= 0; i-- {
		if s := status.Attempts[i].Result.Summary; s != "" {
			return s
		}
	}
	return ""
}

// truncate shortens s to at most n runes, adding an ellipsis when it cuts.
func truncate(s string, n int) string {
	r := []rune(s)
	if len(r) <= n {
		return s
	}
	return string(r[:n]) + "…"
}

// shortID is an id's first 8 characters, the short-id convention of the UI.
func shortID(id string) string {
	if len(id) > 8 {
		return id[:8]
	}
	return id
}
