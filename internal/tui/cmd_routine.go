package tui

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"strings"
	"text/tabwriter"

	"github.com/BurntSushi/toml"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

func RunRoutine(ctx context.Context, c *Context, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		// --help on a parent command is a request, not a mistake.
		code := 2
		if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
			code = 0
		}
		fmt.Fprintln(c.Stderr, "usage: forge routine add|list|show|edit|run|enable|disable NAME [flags]")
		return code
	}
	sub, rest := args[0], args[1:]
	switch sub {
	case "add":
		return runRoutineAdd(ctx, c, rest)
	case "list":
		return runRoutineList(ctx, c, rest)
	case "show", "run", "enable", "disable", "edit":
		return runRoutineNamed(ctx, c, sub, rest)
	}
	fmt.Fprintf(c.Stderr, "forge routine: unknown subcommand %q\n", sub)
	return 2
}

// routineFlags are the common fields; --from takes a TOML file for the rest.
func routineFlags(fs interface {
	String(string, string, string) *string
	Int(string, int, string) *int
	Float64(string, float64, string) *float64
	Bool(string, bool, string) *bool
}) func(r *store.Routine) {
	mode := fs.String("mode", "run", "mode")
	prompt := fs.String("prompt", "", "prompt ({{repo}} allowed)")
	repos := fs.String("repos", "", "comma-separated repository names")
	modelAlias := fs.String("model", "haiku", "model alias")
	effort := fs.String("effort", "", "effort level")
	maxTurns := fs.Int("max-turns", 30, "max turns")
	timeout := fs.Int("timeout", 1800, "timeout in seconds")
	budget := fs.Float64("max-budget-usd", 0, "cap in USD (0 = none)")
	schedule := fs.String("schedule", "", "cron schedule")
	autonomy := fs.String("autonomy", "", "ask|checkpoint|notify|auto")
	class := fs.String("class", "normal", "budget class")
	priority := fs.Int("priority", 50, "priority")
	concurrency := fs.Int("concurrency", 1, "max active runs")
	from := fs.String("from", "", "TOML file with any routine field")
	return func(r *store.Routine) {
		if *from != "" {
			if _, err := toml.DecodeFile(*from, r); err != nil {
				r.Name = "" // signal the error through validation
				return
			}
		}
		r.Mode, r.Model, r.Effort, r.MaxTurns, r.TimeoutSeconds, r.MaxBudgetUSD = or(r.Mode, *mode), or(r.Model, *modelAlias), or(r.Effort, *effort), orInt(r.MaxTurns, *maxTurns), orInt(r.TimeoutSeconds, *timeout), r.MaxBudgetUSD+*budget
		r.Schedule, r.Autonomy, r.BudgetClass = or(r.Schedule, *schedule), model.Autonomy(or(string(r.Autonomy), *autonomy)), model.BudgetClass(or(string(r.BudgetClass), *class))
		r.Priority, r.Concurrency = orInt(r.Priority, *priority), orInt(r.Concurrency, *concurrency)
		if *prompt != "" {
			r.Prompt = *prompt
		}
		if *repos != "" {
			r.Repositories = strings.Split(*repos, ",")
		}
		r.ScheduleEnabled = r.Schedule != ""
	}
}

func or(a, b string) string {
	if a != "" {
		return a
	}
	return b
}

func orInt(a, b int) int {
	if a != 0 {
		return a
	}
	return b
}

func runRoutineAdd(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("routine add")
	apply := routineFlags(fs)
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.Stderr, "usage: forge routine add NAME --prompt … --repos a,b [flags]")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.routine")
	if code >= 0 {
		return code
	}
	r := store.Routine{Name: fs.Arg(0)}
	apply(&r)
	r.Name = fs.Arg(0)
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("routine add", err)
	}
	var out store.Routine
	if err := cl.Do(ctx, http.MethodPost, "/api/v1/routines", r, &out); err != nil {
		return c.Fail("routine add", err)
	}
	fmt.Fprintf(c.Stdout, "routine %s created (generation %d)\n", out.Name, out.Generation)
	return 0
}

func runRoutineList(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("routine list")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.ResolveLogging(lf, "cli.routine")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("routine list", err)
	}
	var out []store.Routine
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/routines", nil, &out); err != nil {
		return c.Fail("routine list", err)
	}
	if *asJSON {
		c.PrintJSON(out)
		return 0
	}
	tw := tabwriter.NewWriter(c.Stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "NAME\tGEN\tMODE\tMODEL\tREPOS\tCLASS\tSCHEDULE")
	for _, r := range out {
		sched := r.Schedule
		if sched != "" && !r.ScheduleEnabled {
			sched += " (off)"
		}
		fmt.Fprintf(tw, "%s\t%d\t%s\t%s\t%s\t%s\t%s\n", r.Name, r.Generation, r.Mode, r.Model, strings.Join(r.Repositories, ","), r.BudgetClass, sched)
	}
	if err := tw.Flush(); err != nil {
		return c.Fail("routine list", err)
	}
	return 0
}

func runRoutineNamed(ctx context.Context, c *Context, sub string, args []string) int {
	fs, lf := c.Flags("routine " + sub)
	asJSON := fs.Bool("json", false, "JSON output")
	var repos multiFlag
	fs.Var(&repos, "repo", "narrow a run to these repositories (run only)")
	from := fs.String("from", "", "apply a TOML file instead of $EDITOR (edit only)")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintf(c.Stderr, "usage: forge routine %s NAME\n", sub)
		return 2
	}
	name := fs.Arg(0)
	_, log, code := c.ResolveLogging(lf, "cli.routine")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("routine "+sub, err)
	}
	var r store.Routine
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/routines/"+name, nil, &r); err != nil {
		return c.Fail("routine "+sub, err)
	}
	switch sub {
	case "show":
		if *asJSON {
			c.PrintJSON(r)
			return 0
		}
		var b strings.Builder
		if err := toml.NewEncoder(&b).Encode(r); err != nil {
			return c.Fail("routine show", err)
		}
		fmt.Fprint(c.Stdout, b.String())
		return 0
	case "run":
		var out taskView
		body := map[string]any{}
		if len(repos) > 0 {
			body["repositories"] = []string(repos)
		}
		if err := cl.Do(ctx, http.MethodPost, "/api/v1/routines/"+name+"/run", body, &out); err != nil {
			return c.Fail("routine run", err)
		}
		fmt.Fprintf(c.Stdout, "task %s created from %s@%d (%d target(s))\n", short(out.Work.ID), name, r.Generation, len(out.Targets))
		return 0
	case "enable", "disable":
		r.ScheduleEnabled = sub == "enable"
		if r.ScheduleEnabled && r.Schedule == "" {
			fmt.Fprintln(c.Stderr, "forge routine enable: the routine has no schedule")
			return 2
		}
		if err := cl.Do(ctx, http.MethodPut, fmt.Sprintf("/api/v1/routines/%s?generation=%d", name, r.Generation), r, &r); err != nil {
			return c.Fail("routine "+sub, err)
		}
		fmt.Fprintf(c.Stdout, "routine %s schedule %sd (generation %d)\n", name, sub, r.Generation)
		return 0
	case "edit":
		edited, err := editRoutine(ctx, c, r, *from)
		if err != nil {
			return c.Fail("routine edit", err)
		}
		if err := cl.Do(ctx, http.MethodPut, fmt.Sprintf("/api/v1/routines/%s?generation=%d", name, r.Generation), edited, &edited); err != nil {
			return c.Fail("routine edit", err)
		}
		fmt.Fprintf(c.Stdout, "routine %s updated (generation %d)\n", name, edited.Generation)
		return 0
	}
	return 2
}

// editRoutine opens the routine as TOML in $EDITOR (or reads --from) and
// returns the result; the generation the user saw goes back for the 409 check.
func editRoutine(ctx context.Context, c *Context, r store.Routine, from string) (store.Routine, error) {
	if from != "" {
		if _, err := toml.DecodeFile(from, &r); err != nil {
			return r, err
		}
		return r, nil
	}
	var edited store.Routine
	if err := editTOML(ctx, c, "forge-routine-*.toml", r, &edited); err != nil {
		return r, err
	}
	edited.Name = r.Name
	return edited, nil
}

// editTOML round-trips a value through $EDITOR as a TOML temp file.
func editTOML(ctx context.Context, c *Context, pattern string, in, out any) error {
	editor := c.Getenv("EDITOR")
	if editor == "" {
		return fmt.Errorf("$EDITOR is not set; use --from FILE.toml")
	}
	f, err := os.CreateTemp("", pattern)
	if err != nil {
		return err
	}
	path := f.Name()
	defer func() {
		if rerr := os.Remove(path); rerr != nil {
			fmt.Fprintln(c.Stderr, "edit: remove temp file:", rerr)
		}
	}()
	if err := toml.NewEncoder(f).Encode(in); err != nil {
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	cmd := exec.CommandContext(ctx, editor, path)
	cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, os.Stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("%s: %w", editor, err)
	}
	if _, err := toml.DecodeFile(path, out); err != nil {
		return err
	}
	return nil
}
