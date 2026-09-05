package tui

// `forge bench` runs and tracks end-to-end benchmarks: a spec's objective is
// submitted as a plan-mode root against a throwaway repository, the
// recursive loop (plan → batch → supervise → revise) does the rest, and the
// run's rollup — score, cost, wall time — lands as one row of the
// benchmark's history. The benchmark IS the eval: if quality-at-fixed-budget
// does not trend up across runs as the library and KB accrete, the learning
// loop is not real.

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"text/tabwriter"
	"time"
)

// benchSpec is bench/specs/<name>.md: minimal frontmatter over the objective.
type benchSpec struct {
	Size     string // S|M|L, default L
	Model    string // model alias for the plan (tasks inherit), default sonnet
	Autonomy string // default auto
	Body     string
}

// RunBench is `forge bench run NAME [--spec path] [--dir path] [--no-wait]`
// and `forge bench list NAME`.
func RunBench(ctx context.Context, c *Context, args []string) int {
	if len(args) < 2 {
		fmt.Fprintln(c.Stderr, "usage: forge bench run NAME [--spec path] [--dir path] [--no-wait] | list NAME")
		return 2
	}
	switch args[0] {
	case "run":
		return runBenchRun(ctx, c, args[1:])
	case "list":
		return runBenchList(ctx, c, args[1:])
	}
	fmt.Fprintln(c.Stderr, "usage: forge bench run NAME [--spec path] [--dir path] [--no-wait] | list NAME")
	return 2
}

func runBenchRun(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("bench run")
	specPath := fs.String("spec", "", "spec file (default bench/specs/<name>.md)")
	dir := fs.String("dir", "", "parent directory for the throwaway repo (default ~/Projects)")
	noWait := fs.Bool("no-wait", false, "submit and return without polling")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.Stderr, "usage: forge bench run NAME [--spec path] [--dir path] [--no-wait]")
		return 2
	}
	name := fs.Arg(0)
	_, log, code := c.ResolveLogging(lf, "cli.bench")
	if code >= 0 {
		return code
	}
	path := *specPath
	if path == "" {
		path = filepath.Join("bench", "specs", name+".md")
	}
	spec, err := loadBenchSpec(path)
	if err != nil {
		return c.Fail("bench run", err)
	}
	parent := *dir
	if parent == "" {
		parent = filepath.Join(c.UserHome, "Projects")
	}
	repoName := fmt.Sprintf("bench-%s-%s", name, time.Now().Format("20060102-1504"))
	repoPath := filepath.Join(parent, repoName)
	if err := initBenchRepo(ctx, repoPath); err != nil {
		return c.Fail("bench run", err)
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("bench run", err)
	}
	if err := cl.Do(ctx, http.MethodPost, "/api/v1/repositories", map[string]any{"path": repoPath}, nil); err != nil {
		return c.Fail("bench run", fmt.Errorf("register %s: %w", repoPath, err))
	}
	var created struct {
		Work struct {
			ID string `json:"id"`
		} `json:"work"`
	}
	req := map[string]any{
		"prompt": spec.Body, "repositories": []string{repoName}, "mode": "plan",
		"size": spec.Size, "model": spec.Model, "autonomy": spec.Autonomy,
		"integrate": true, "bench_name": name, "title": "bench: " + name, "force": true,
	}
	if err := cl.Do(ctx, http.MethodPost, "/api/v1/tasks", req, &created); err != nil {
		return c.Fail("bench run", err)
	}
	fmt.Fprintf(c.Stdout, "bench %s: root work %s in %s (repo %s)\n", name, created.Work.ID, repoPath, repoName)
	if *noWait {
		fmt.Fprintf(c.Stdout, "track it: forge bench list %s · /work/%s\n", name, created.Work.ID)
		return 0
	}
	fmt.Fprintln(c.Stdout, "waiting for the tree to settle (Ctrl-C detaches; the run keeps going)…")
	for {
		select {
		case <-ctx.Done():
			return 0
		case <-time.After(30 * time.Second):
		}
		var lin struct {
			Rollup benchRollup `json:"rollup"`
		}
		if err := cl.Do(ctx, http.MethodGet, "/api/v1/work/"+created.Work.ID+"/lineage", nil, &lin); err != nil {
			fmt.Fprintf(c.Stderr, "poll: %v\n", err)
			continue
		}
		if lin.Rollup.Open > 0 {
			fmt.Fprintf(c.Stdout, "  %d works, %d open, $%.2f so far\n", lin.Rollup.Works, lin.Rollup.Open, lin.Rollup.CostUSD)
			continue
		}
		printRollup(c, lin.Rollup)
		return 0
	}
}

type benchRollup struct {
	Works         int            `json:"works"`
	Attempts      int            `json:"attempts"`
	CostUSD       float64        `json:"cost_usd"`
	WallSeconds   float64        `json:"wall_seconds"`
	OutcomeCounts map[string]int `json:"outcome_counts"`
	Scores        map[string]int `json:"scores"`
	Weakness      string         `json:"weakness"`
	Open          int            `json:"open"`
}

func printRollup(c *Context, r benchRollup) {
	score := "-"
	if v, ok := r.Scores["overall"]; ok {
		score = fmt.Sprintf("%d/5", v)
	}
	fmt.Fprintf(c.Stdout, "settled: score %s · $%.2f · %.0fm wall · %d works, %d attempts · outcomes %v\n",
		score, r.CostUSD, r.WallSeconds/60, r.Works, r.Attempts, r.OutcomeCounts)
	if r.Weakness != "" {
		fmt.Fprintf(c.Stdout, "weakness: %s\n", r.Weakness)
	}
}

func runBenchList(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("bench list")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.Stderr, "usage: forge bench list NAME")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.bench")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("bench list", err)
	}
	var runs []struct {
		Work struct {
			ID        string    `json:"id"`
			CreatedAt time.Time `json:"created_at"`
		} `json:"work"`
		State  string      `json:"state"`
		Rollup benchRollup `json:"rollup"`
	}
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/bench/"+fs.Arg(0), nil, &runs); err != nil {
		return c.Fail("bench list", err)
	}
	if len(runs) == 0 {
		fmt.Fprintln(c.Stdout, "no runs yet — forge bench run "+fs.Arg(0))
		return 0
	}
	w := tabwriter.NewWriter(c.Stdout, 2, 4, 2, ' ', 0)
	fmt.Fprintln(w, "STARTED\tWORK\tSTATE\tSCORE\tCOST\tWALL\tWORKS\tWEAKNESS")
	for _, r := range runs {
		score := "-"
		if v, ok := r.Rollup.Scores["overall"]; ok {
			score = fmt.Sprintf("%d/5", v)
		}
		wall := "-"
		if r.Rollup.WallSeconds > 0 {
			wall = fmt.Sprintf("%.0fm", r.Rollup.WallSeconds/60)
		}
		weak := r.Rollup.Weakness
		if len(weak) > 60 {
			weak = weak[:60] + "…"
		}
		fmt.Fprintf(w, "%s\t%.8s\t%s\t%s\t$%.2f\t%s\t%d\t%s\n",
			r.Work.CreatedAt.Format("2006-01-02 15:04"), r.Work.ID, r.State, score, r.Rollup.CostUSD, wall, r.Rollup.Works, weak)
	}
	if err := w.Flush(); err != nil {
		return c.Fail("bench list", err)
	}
	return 0
}

// loadBenchSpec parses minimal frontmatter (size/model/autonomy) over the
// objective body.
func loadBenchSpec(path string) (*benchSpec, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("spec: %w", err)
	}
	spec := &benchSpec{Size: "L", Model: "sonnet", Autonomy: "auto", Body: string(raw)}
	body := string(raw)
	if strings.HasPrefix(body, "---\n") {
		end := strings.Index(body[4:], "\n---")
		if end < 0 {
			return nil, fmt.Errorf("spec %s: unterminated frontmatter", path)
		}
		for _, line := range strings.Split(body[4:4+end], "\n") {
			k, v, ok := strings.Cut(line, ":")
			if !ok {
				continue
			}
			v = strings.TrimSpace(v)
			switch strings.TrimSpace(k) {
			case "size":
				spec.Size = v
			case "model":
				spec.Model = v
			case "autonomy":
				spec.Autonomy = v
			default:
				return nil, fmt.Errorf("spec %s: unknown key %q", path, strings.TrimSpace(k))
			}
		}
		spec.Body = strings.TrimSpace(strings.TrimPrefix(body[4+end:], "\n---\n"))
	}
	if strings.TrimSpace(spec.Body) == "" {
		return nil, fmt.Errorf("spec %s: empty objective", path)
	}
	return spec, nil
}

// initBenchRepo creates the throwaway checkout: git init on main, one empty
// commit, a self-referencing origin (repositories require one; the library
// repo set the precedent).
func initBenchRepo(ctx context.Context, path string) error {
	if _, err := os.Stat(path); err == nil {
		return fmt.Errorf("%s already exists", path)
	}
	if err := os.MkdirAll(path, 0o755); err != nil {
		return err
	}
	git := func(args ...string) error {
		out, err := exec.CommandContext(ctx, "git", append([]string{"-C", path}, args...)...).CombinedOutput()
		if err != nil {
			return fmt.Errorf("git %s: %s", strings.Join(args, " "), strings.TrimSpace(string(out)))
		}
		return nil
	}
	if err := git("init", "-q", "-b", "main"); err != nil {
		return err
	}
	if err := git("-c", "user.name=forge", "-c", "user.email=forge@localhost", "commit", "-q", "--allow-empty", "-m", "bench: empty start"); err != nil {
		return err
	}
	return git("remote", "add", "origin", path)
}
