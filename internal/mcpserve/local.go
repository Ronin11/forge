// The repository tools run inside forge mcp itself (DESIGN.md §13): this
// process is a child of the agent, in the worktree and the agent's process
// group, so a group kill takes its checks with it and the control plane never
// spawns processes in a worker-owned directory.

package mcpserve

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"syscall"
	"time"

	"forge/internal/core/worker"
)

// gitTimeout caps each git command a local tool runs; every command here reads
// local state only, so 30 s means something is wrong, not slow.
const gitTimeout = 30 * time.Second

// localTool is one in-process tool: its fallback input schema (used when the
// daemon's listing carries none) and its implementation.
type localTool struct {
	schema json.RawMessage
	run    func(ctx context.Context, b *Bridge, input json.RawMessage) (json.RawMessage, bool)
}

// localTools returns the repository tool table. It is a fresh value per
// bridge, never package state.
func localTools() map[string]localTool {
	emptyObject := json.RawMessage(`{"type":"object","properties":{},"additionalProperties":false}`)
	return map[string]localTool{
		"forge_repo_status": {
			schema: emptyObject,
			run: func(ctx context.Context, b *Bridge, input json.RawMessage) (json.RawMessage, bool) {
				return b.repoStatus(ctx, input)
			},
		},
		"forge_check": {
			schema: json.RawMessage(`{"type":"object","properties":{"check":{"type":"string","description":"run only this declared check; omit to run all"}},"additionalProperties":false}`),
			run: func(ctx context.Context, b *Bridge, input json.RawMessage) (json.RawMessage, bool) {
				return b.runCheck(ctx, input)
			},
		},
		"forge_diff_summary": {
			schema: emptyObject,
			run: func(ctx context.Context, b *Bridge, input json.RawMessage) (json.RawMessage, bool) {
				return b.diffSummary(ctx, input)
			},
		},
	}
}

// errText is a failed tool call: the message becomes the isError text content.
func errText(err error) (json.RawMessage, bool) {
	return json.RawMessage(err.Error()), true
}

// okJSON encodes a successful tool result.
func okJSON(v any) (json.RawMessage, bool) {
	b, err := json.Marshal(v)
	if err != nil {
		return errText(fmt.Errorf("encode result: %w", err))
	}
	return b, false
}

// repoStatusResult is forge_repo_status's output.
type repoStatusResult struct {
	SchemaVersion int      `json:"schema_version"`
	Repository    string   `json:"repository"`
	Branch        string   `json:"branch"`
	BaseBranch    string   `json:"base_branch"`
	BaseCommit    string   `json:"base_commit"`
	Head          string   `json:"head"`
	Dirty         bool     `json:"dirty"`
	Ahead         int      `json:"ahead"`
	ChangedPaths  []string `json:"changed_paths"`
}

func (b *Bridge) repoStatus(ctx context.Context, _ json.RawMessage) (json.RawMessage, bool) {
	wt := b.attempt.WorktreePath
	statusOut, err := runGit(ctx, wt, "--no-optional-locks", "status", "--porcelain=v1")
	if err != nil {
		return errText(err)
	}
	paths := porcelainPaths(statusOut)
	head, err := runGit(ctx, wt, "rev-parse", "HEAD")
	if err != nil {
		return errText(err)
	}
	countOut, err := runGit(ctx, wt, "rev-list", "--count", b.attempt.BaseCommit+"..HEAD")
	if err != nil {
		return errText(err)
	}
	ahead, err := strconv.Atoi(strings.TrimSpace(countOut))
	if err != nil {
		return errText(fmt.Errorf("parse rev-list count %q: %w", strings.TrimSpace(countOut), err))
	}
	return okJSON(repoStatusResult{
		SchemaVersion: schemaVersion,
		Repository:    b.attempt.Repository,
		Branch:        b.attempt.Branch,
		BaseBranch:    b.attempt.BaseBranch,
		BaseCommit:    b.attempt.BaseCommit,
		Head:          strings.TrimSpace(head),
		Dirty:         len(paths) > 0,
		Ahead:         ahead,
		ChangedPaths:  paths,
	})
}

// checkResult is forge_check's output. Declared reports whether the repository
// declares any checks at all, so "passed with zero checks" is never mistaken
// for verification.
type checkResult struct {
	SchemaVersion int                  `json:"schema_version"`
	Declared      bool                 `json:"declared"`
	Checks        []worker.CheckResult `json:"checks"`
}

func (b *Bridge) runCheck(ctx context.Context, input json.RawMessage) (json.RawMessage, bool) {
	var in struct {
		Check string `json:"check"`
	}
	if err := json.Unmarshal(input, &in); err != nil {
		return errText(fmt.Errorf("decode input: %w", err))
	}
	wt := b.attempt.WorktreePath
	ft, err := worker.ReadForgeToml(wt)
	if err != nil {
		return errText(err)
	}
	declared := ft != nil && len(ft.Checks) > 0
	if in.Check != "" {
		if ft == nil || ft.Checks[in.Check] == nil {
			return json.RawMessage("check " + in.Check + " is not declared"), true
		}
		one := *ft
		one.Checks = map[string][]string{in.Check: ft.Checks[in.Check]}
		ft = &one
	}
	results := worker.RunChecks(ctx, wt, ft, worker.PassthroughEnv(os.Environ()))
	if results == nil {
		results = []worker.CheckResult{}
	}
	return okJSON(checkResult{SchemaVersion: schemaVersion, Declared: declared, Checks: results})
}

// diffFile is one file of forge_diff_summary. Insertions/deletions are -1 when
// unknown: binary files, and every uncommitted entry.
type diffFile struct {
	Path        string `json:"path"`
	Insertions  int    `json:"insertions"`
	Deletions   int    `json:"deletions"`
	Uncommitted bool   `json:"uncommitted,omitempty"`
}

// diffSummaryResult is forge_diff_summary's output; totals count committed
// lines only (unknown counts never sum).
type diffSummaryResult struct {
	SchemaVersion   int        `json:"schema_version"`
	Base            string     `json:"base"`
	Head            string     `json:"head"`
	Files           []diffFile `json:"files"`
	TotalInsertions int        `json:"total_insertions"`
	TotalDeletions  int        `json:"total_deletions"`
}

func (b *Bridge) diffSummary(ctx context.Context, _ json.RawMessage) (json.RawMessage, bool) {
	wt := b.attempt.WorktreePath
	numstat, err := runGit(ctx, wt, "diff", "--numstat", b.attempt.BaseCommit+"..HEAD")
	if err != nil {
		return errText(err)
	}
	files := []diffFile{}
	totalIns, totalDel := 0, 0
	for line := range strings.Lines(numstat) {
		parts := strings.SplitN(strings.TrimRight(line, "\n"), "\t", 3)
		if len(parts) != 3 {
			continue
		}
		ins, del := numstatCount(parts[0]), numstatCount(parts[1])
		if ins > 0 {
			totalIns += ins
		}
		if del > 0 {
			totalDel += del
		}
		files = append(files, diffFile{Path: parts[2], Insertions: ins, Deletions: del})
	}
	statusOut, err := runGit(ctx, wt, "--no-optional-locks", "status", "--porcelain=v1")
	if err != nil {
		return errText(err)
	}
	for _, path := range porcelainPaths(statusOut) {
		files = append(files, diffFile{Path: path, Insertions: -1, Deletions: -1, Uncommitted: true})
	}
	head, err := runGit(ctx, wt, "rev-parse", "HEAD")
	if err != nil {
		return errText(err)
	}
	return okJSON(diffSummaryResult{
		SchemaVersion:   schemaVersion,
		Base:            b.attempt.BaseCommit,
		Head:            strings.TrimSpace(head),
		Files:           files,
		TotalInsertions: totalIns,
		TotalDeletions:  totalDel,
	})
}

// numstatCount parses one numstat column; git prints "-" for binary files,
// which becomes -1 (unknown).
func numstatCount(s string) int {
	if s == "-" {
		return -1
	}
	n, err := strconv.Atoi(s)
	if err != nil {
		return -1
	}
	return n
}

// porcelainPaths extracts the paths of a `git status --porcelain=v1` listing;
// for a rename it keeps the new name.
func porcelainPaths(out string) []string {
	paths := []string{}
	for line := range strings.Lines(out) {
		line = strings.TrimRight(line, "\n")
		if len(line) < 4 {
			continue
		}
		path := line[3:]
		if _, to, ok := strings.Cut(path, " -> "); ok {
			path = to
		}
		paths = append(paths, path)
	}
	return paths
}

// runGit runs one git command in the worktree: its own process group, a 30 s
// timeout, and no credential prompts, mirroring the worker's git discipline.
func runGit(ctx context.Context, worktree string, args ...string) (string, error) {
	cctx, cancel := context.WithTimeout(ctx, gitTimeout)
	defer cancel()
	cmd := exec.CommandContext(cctx, "git", args...)
	cmd.Dir = worktree
	cmd.Env = append(os.Environ(), "GIT_TERMINAL_PROMPT=0")
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	cmd.Cancel = func() error { return syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL) }
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		msg := strings.TrimSpace(stderr.String())
		if len(msg) > 2048 {
			msg = msg[len(msg)-2048:]
		}
		return "", fmt.Errorf("git %s: %w: %s", args[0], err, msg)
	}
	return stdout.String(), nil
}
