package main

import (
	"strings"
	"testing"
)

func TestDispatch(t *testing.T) {
	cases := []struct {
		name       string
		args       []string
		wantCode   int
		wantStdout string
		wantStderr string
	}{
		{"no args", nil, 2, "", "usage: forge"},
		{"help", []string{"help"}, 0, "usage: forge", ""},
		{"dash h", []string{"-h"}, 0, "version    print", ""},
		{"version", []string{"version"}, 0, "forge dev", ""},
		{"version -h", []string{"version", "-h"}, 0, "usage: forge version", ""},
		{"version extra", []string{"version", "x"}, 2, "", "unexpected argument"},
		{"unknown", []string{"bogus"}, 2, "", `unknown command "bogus"`},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			var stdout, stderr strings.Builder
			got := dispatch(commands(), c.args, &stdout, &stderr)
			if got != c.wantCode {
				t.Errorf("exit = %d, want %d", got, c.wantCode)
			}
			if !strings.Contains(stdout.String(), c.wantStdout) {
				t.Errorf("stdout = %q, want it to contain %q", stdout.String(), c.wantStdout)
			}
			if !strings.Contains(stderr.String(), c.wantStderr) {
				t.Errorf("stderr = %q, want it to contain %q", stderr.String(), c.wantStderr)
			}
			if c.wantStdout == "" && stdout.Len() != 0 {
				t.Errorf("unexpected stdout %q", stdout.String())
			}
		})
	}
}
