package integrator

import (
	"errors"
	"os"
	"regexp"
	"strings"
	"testing"

	"forge/internal/core/worker"
)

func TestAllowedPushBranch(t *testing.T) {
	ft := &worker.ForgeToml{IntegrationBranch: "main", TaskBranches: "forge/*"}
	cases := []struct {
		branch string
		ft     *worker.ForgeToml
		ok     bool
	}{
		{"main", ft, true},
		{"forge/x-123", ft, true},
		{"master", ft, false},
		{"main", nil, false},
		{"main", &worker.ForgeToml{}, false}, // no integration_branch declared
		{"", ft, false},
		{"-option", ft, false},
		{"+main", ft, false},
		{"main:evil", ft, false},
		{"forge/deep/branch", ft, false}, // task_branches glob does not cross a slash
	}
	for _, tc := range cases {
		err := AllowedPushBranch(tc.branch, tc.ft)
		if tc.ok && err != nil {
			t.Errorf("AllowedPushBranch(%q) = %v, want allowed", tc.branch, err)
		}
		if !tc.ok {
			if err == nil {
				t.Errorf("AllowedPushBranch(%q) allowed, want refused", tc.branch)
			} else if !errors.Is(err, ErrPushRefused) {
				t.Errorf("AllowedPushBranch(%q) = %v, want ErrPushRefused", tc.branch, err)
			}
		}
	}
}

// Constitution 10's "--force is impossible by construction": the source that
// builds the push invocation must never contain a force flag or a +refspec.
// The whole package is grepped so a second push call cannot sneak one in.
func TestNoForceInPushSource(t *testing.T) {
	entries, err := os.ReadDir(".")
	if err != nil {
		t.Fatal(err)
	}
	pushLine := regexp.MustCompile(`"push"`)
	literal := regexp.MustCompile(`"(?:[^"\\]|\\.)*"`)
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".go") || strings.HasSuffix(e.Name(), "_test.go") {
			continue
		}
		src, err := os.ReadFile(e.Name())
		if err != nil {
			t.Fatal(err)
		}
		text := string(src)
		for _, forbidden := range []string{`"--force"`, `"-f"`, `"--force-with-lease"`, `"+refs/`, `"+HEAD`} {
			if strings.Contains(text, forbidden) {
				t.Errorf("%s contains %s", e.Name(), forbidden)
			}
		}
		for i, line := range strings.Split(text, "\n") {
			if !pushLine.MatchString(line) {
				continue
			}
			// Every string literal on a push invocation line: no force
			// flag and no +refspec may appear as an argument.
			for _, lit := range literal.FindAllString(line, -1) {
				arg := strings.Trim(lit, `"`)
				if arg == "-f" || strings.HasPrefix(arg, "--force") || strings.HasPrefix(arg, "+") {
					t.Errorf("%s:%d: push argument %q is forbidden by constitution 10", e.Name(), i+1, arg)
				}
			}
		}
	}
}
