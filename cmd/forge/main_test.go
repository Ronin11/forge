package main

import (
	"context"
	"forge/internal/tui"
	"strings"
	"testing"
	"time"
)

func run(args []string, env map[string]string) (code int, stdout, stderr string) {
	var out, errOut strings.Builder
	c := &tui.Context{Stdout: &out, Stderr: &errOut, Getenv: func(k string) string { return env[k] }, ForgeHome: "/tmp/forge-test-home", UserHome: "/tmp/forge-test-user", Now: time.Now}
	code = dispatch(context.Background(), commands(), c, args)
	return code, out.String(), errOut.String()
}

func TestDispatch(t *testing.T) {
	cases := []struct {
		name       string
		args       []string
		env        map[string]string
		wantCode   int
		wantStdout string
		wantStderr string
	}{
		{"no args", nil, nil, 2, "", "usage: forge"},
		{"help", []string{"help"}, nil, 0, "usage: forge", ""},
		{"dash h", []string{"-h"}, nil, 0, "version    print", ""},
		{"version", []string{"version"}, nil, 0, "forge dev", ""},
		{"version -h", []string{"version", "-h"}, nil, 0, "usage: forge version", ""},
		{"version extra", []string{"version", "x"}, nil, 2, "", "unexpected argument"},
		{"version bad flag", []string{"version", "--bogus"}, nil, 2, "", "flag provided but not defined"},
		{"version -h flags", []string{"version", "--help"}, nil, 0, "-log-level", ""},
		{"unknown", []string{"bogus"}, nil, 2, "", `unknown command "bogus"`},
		{"-v shows debug line", []string{"version", "-v"}, nil, 0, "forge dev", "printing version"},
		{"env level shows debug line", []string{"version"}, map[string]string{"FORGE_LOG_LEVEL": "debug"}, 0, "forge dev", "printing version"},
		{"flag beats env", []string{"version", "--log-level", "warn"}, map[string]string{"FORGE_LOG_LEVEL": "debug"}, 0, "forge dev", ""},
		{"json format", []string{"version", "-v", "--log-format=json"}, nil, 0, "forge dev", `"component":"cli.version"`},
		{"bad level", []string{"version", "--log-level", "loud"}, nil, 2, "", "unknown log level"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			code, stdout, stderr := run(c.args, c.env)
			if code != c.wantCode {
				t.Errorf("exit = %d, want %d (stderr %q)", code, c.wantCode, stderr)
			}
			if !strings.Contains(stdout, c.wantStdout) {
				t.Errorf("stdout = %q, want it to contain %q", stdout, c.wantStdout)
			}
			if !strings.Contains(stderr, c.wantStderr) {
				t.Errorf("stderr = %q, want it to contain %q", stderr, c.wantStderr)
			}
			if c.wantStderr == "" && stderr != "" {
				t.Errorf("unexpected stderr %q", stderr)
			}
			if c.wantCode == 2 && stdout != "" {
				t.Errorf("usage error wrote to Stdout: %q", stdout)
			}
		})
	}
}
