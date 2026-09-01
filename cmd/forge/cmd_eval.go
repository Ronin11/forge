// forge eval runs the golden cases under evals/ through the fake-claude
// executor and scores them (DESIGN.md §23); --record-proposal posts the
// summary score onto a proposal so it can be approved.
package main

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"text/tabwriter"
	"time"

	"forge/internal/core/eval"
)

func runEval(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("eval")
	mode := fs.String("mode", "", "mode whose cases run (required)")
	modelAlias := fs.String("model", "haiku", "model alias submitted with each case")
	cases := fs.String("cases", "evals", "cases directory (holds <mode>/<case>/eval.toml)")
	fixtures := fs.String("fixtures", filepath.Join("testdata", "fixtures"), "fake-claude fixtures directory")
	promptVersion := fs.String("prompt-version", "", "prompt version hash recorded in the report")
	timeout := fs.Int("timeout", 120, "seconds each case may take")
	recordProposal := fs.String("record-proposal", "", "proposal id to record the summary score on")
	asJSON := fs.Bool("json", false, "JSON report")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if *mode == "" {
		fmt.Fprintln(c.stderr, "forge eval: --mode is required")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.eval")
	if code >= 0 {
		return code
	}
	forgeBin, err := os.Executable()
	if err != nil {
		return c.fail("eval", fmt.Errorf("resolve forge binary: %w", err))
	}
	casesDir, err := filepath.Abs(*cases)
	if err != nil {
		return c.fail("eval", err)
	}
	fixturesDir, err := filepath.Abs(*fixtures)
	if err != nil {
		return c.fail("eval", err)
	}
	workDir, err := os.MkdirTemp("", "forge-eval-")
	if err != nil {
		return c.fail("eval", err)
	}
	defer func() {
		if rerr := os.RemoveAll(workDir); rerr != nil {
			fmt.Fprintln(c.stderr, "forge eval: clean work dir:", rerr)
		}
	}()
	rep, err := eval.Run(ctx, eval.Options{
		Mode: *mode, Model: *modelAlias, PromptVersionHash: *promptVersion,
		CasesDir: casesDir, FixturesDir: fixturesDir, ForgeBin: forgeBin,
		WorkDir: workDir, Timeout: time.Duration(*timeout) * time.Second, Logger: log,
	})
	if err != nil {
		return c.fail("eval", err)
	}
	failed := 0
	if *asJSON {
		c.printJSON(rep)
	} else {
		tw := tabwriter.NewWriter(c.stdout, 0, 4, 2, ' ', 0)
		fmt.Fprintln(tw, "CASE\tPASS\tSTATE\tTURNS\tCOST\tDETAILS")
		for _, r := range rep.Cases {
			pass := "ok"
			if !r.Pass {
				pass = "FAIL"
			}
			fmt.Fprintf(tw, "%s\t%s\t%s\t%d\t$%.4f\t%s\n", r.Name, pass, r.State, r.Turns, r.CostUSD, r.Details)
		}
		if err := tw.Flush(); err != nil {
			return c.fail("eval", err)
		}
	}
	for _, r := range rep.Cases {
		if !r.Pass {
			failed++
		}
	}
	if !*asJSON {
		fmt.Fprintf(c.stdout, "score: %d/%d passed = %.2f\n", len(rep.Cases)-failed, len(rep.Cases), rep.Score)
	}
	if *recordProposal != "" {
		cl := c.client(log)
		if err := cl.connect(ctx); err != nil {
			return c.fail("eval", err)
		}
		if err := cl.do(ctx, http.MethodPost, "/api/v1/proposals/"+*recordProposal+"/eval", map[string]float64{"score": rep.Score}, nil); err != nil {
			return c.fail("eval", err)
		}
		fmt.Fprintf(c.stdout, "recorded eval score %.2f on proposal %s\n", rep.Score, short(*recordProposal))
	}
	if failed > 0 {
		return 1
	}
	return 0
}
