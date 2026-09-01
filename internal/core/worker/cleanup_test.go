package worker

import "testing"

func TestDecideCleanupTable(t *testing.T) {
	base := CleanupInput{PathExists: true, Registered: true, HeadIsBase: true, Pushed: true, AttemptShortID: "3f9a1c2e"}
	with := func(f func(*CleanupInput)) CleanupInput { in := base; f(&in); return in }
	cases := []struct {
		name   string
		in     CleanupInput
		action CleanupAction
		reason string
	}{
		{"clean and unchanged", base, CleanupRemove, "removed"},
		{"clean and pushed", with(func(i *CleanupInput) { i.HeadIsBase = false }), CleanupRemove, "removed"},
		{"unpushed commits", with(func(i *CleanupInput) { i.HeadIsBase, i.Pushed = false, false }), CleanupRetain, "unpushed commits"},
		{"dirty beats pushed", with(func(i *CleanupInput) { i.Dirty = true }), CleanupRetain, "dirty worktree"},
		{"resumable beats everything", with(func(i *CleanupInput) { i.Resumable, i.Dirty = true, true }), CleanupKeep, "awaiting human answer"},
		{"awaiting merge", with(func(i *CleanupInput) { i.AwaitingMerge, i.Dirty = true, true }), CleanupKeep, "awaiting merge"},
		{"greenfield", with(func(i *CleanupInput) { i.Greenfield = true }), CleanupKeep, "greenfield project"},
		{"missing", with(func(i *CleanupInput) { i.PathExists, i.Registered = false, false }), CleanupMissing, "worktree missing"},
		{"path without registration", with(func(i *CleanupInput) { i.Registered = false }), CleanupRetain, "worktree exists in only one of filesystem and git registry"},
		{"registration without path", with(func(i *CleanupInput) { i.PathExists = false }), CleanupRetain, "worktree exists in only one of filesystem and git registry"},
	}
	for _, c := range cases {
		got := DecideCleanup(c.in)
		if got.Action != c.action || got.Reason != c.reason {
			t.Errorf("%s: got %s/%q want %s/%q", c.name, got.Action, got.Reason, c.action, c.reason)
		}
		if got.Action == CleanupRetain && got.Command != "forge cleanup 3f9a1c2e --confirm" {
			t.Errorf("%s: command %q", c.name, got.Command)
		}
		if (got.Reason == "worktree exists in only one of filesystem and git registry") != got.Inconsistent {
			t.Errorf("%s: inconsistent flag", c.name)
		}
	}
}
