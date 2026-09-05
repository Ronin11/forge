package ui

import (
	"context"
	"net/http"
	"os/exec"
	"regexp"
	"strings"
	"time"
)

// The commit view behind the Learning feed: everything the loop changes is a
// git commit in the prompts library, so "what actually changed" is one
// `git show` away. The page renders the commit header and a classified
// unified diff — no JS, no diff library.

var shaPattern = regexp.MustCompile(`^[0-9a-f]{7,64}$`)

type diffLine struct {
	Class string // file | hunk | add | del | ctx
	Text  string
}

type commitData struct {
	SHA       string
	Subject   string
	Body      string
	Author    string
	At        time.Time
	Stat      string
	Lines     []diffLine
	Truncated bool
	Missing   bool // the sha is not in the library (an unpushed branch's commit)
}

func (u *UI) learningCommit(w http.ResponseWriter, r *http.Request) {
	sha := r.PathValue("sha")
	if !shaPattern.MatchString(sha) {
		http.NotFound(w, r)
		return
	}
	dir := u.libraryDir()
	if dir == "" {
		http.NotFound(w, r)
		return
	}
	data := commitData{SHA: sha}
	ctx, cancel := context.WithTimeout(r.Context(), 5*time.Second)
	defer cancel()

	meta, err := exec.CommandContext(ctx, "git", "-C", dir, "show", "-s",
		"--format=%an%x1f%cI%x1f%s%x1f%b", sha).Output()
	if err != nil {
		// Not an error page: reflect edits that verification refuted live on
		// unpushed branches and never reach the library — say so.
		data.Missing = true
		u.render(w, r, "learning-commit.html", "Commit "+short8(sha), data)
		return
	}
	fields := strings.SplitN(strings.TrimRight(string(meta), "\n"), "\x1f", 4)
	if len(fields) == 4 {
		data.Author, data.Subject, data.Body = fields[0], fields[2], strings.TrimSpace(fields[3])
		if t, err := time.Parse(time.RFC3339, fields[1]); err == nil {
			data.At = t
		}
	}
	if stat, err := exec.CommandContext(ctx, "git", "-C", dir, "show", "--stat=100", "--format=", "--no-color", sha).Output(); err == nil {
		s := string(stat)
		if i := strings.Index(s, "diff --git"); i >= 0 {
			s = s[:i]
		}
		data.Stat = strings.TrimSpace(s)
	}
	patch, err := exec.CommandContext(ctx, "git", "-C", dir, "show", "--format=", "--no-color", sha).Output()
	if err != nil {
		u.fail(w, r, err)
		return
	}
	data.Lines, data.Truncated = classifyDiff(string(patch), 4000)
	u.render(w, r, "learning-commit.html", "Commit "+short8(sha), data)
}

// classifyDiff turns a unified diff into template-renderable lines, capped.
func classifyDiff(patch string, maxLines int) ([]diffLine, bool) {
	var out []diffLine
	for _, line := range strings.Split(patch, "\n") {
		if len(out) >= maxLines {
			return out, true
		}
		class := "ctx"
		switch {
		case strings.HasPrefix(line, "diff --git"), strings.HasPrefix(line, "index "),
			strings.HasPrefix(line, "--- "), strings.HasPrefix(line, "+++ "),
			strings.HasPrefix(line, "new file"), strings.HasPrefix(line, "deleted file"):
			class = "file"
		case strings.HasPrefix(line, "@@"):
			class = "hunk"
		case strings.HasPrefix(line, "+"):
			class = "add"
		case strings.HasPrefix(line, "-"):
			class = "del"
		}
		out = append(out, diffLine{Class: class, Text: line})
	}
	return out, false
}

func short8(s string) string {
	if len(s) > 8 {
		return s[:8]
	}
	return s
}
