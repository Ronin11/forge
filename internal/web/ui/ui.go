package ui

import (
	"bytes"
	"context"
	"embed"
	"encoding/json"
	"errors"
	"fmt"
	"html/template"
	"io/fs"
	"log/slog"
	"net/http"
	"sort"
	"strconv"
	"strings"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/directives"
	"forge/internal/core/engine"
	"forge/internal/core/model"
	"forge/internal/core/plugin"
	"forge/internal/core/protocol"
	"forge/internal/core/stats"
	"forge/internal/core/store"
	"forge/internal/web"
)

//go:embed *.html static/*
var uiFS embed.FS

// UI serves the operator pages: html/template over the store, one stylesheet,
// vanilla JS. It is mounted beside the API on the same listeners.
type UI struct {
	store *store.Store
	log   *slog.Logger
	clock func() time.Time
	tmpl  *template.Template
	mux   *http.ServeMux
	// prompts returns the daemon's last-good persona/fragment library; nil
	// (tests, a bare UI) renders the Prompts page with routines only.
	prompts func() *directives.Library
	// pluginHealth is the supervisor's live view for the System page; nil
	// (tests, a UI without a daemon) renders installed rows as not running.
	pluginHealth func() []plugin.PluginHealth
	// appStatus is the run supervisor's live app state for the Repos list;
	// nil (tests, a bare UI) shows no app chips.
	appStatus func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)
	// attention + quietHours let the Human Queue show each non-critical
	// question's countdown to auto-decision (the same deadline the sweep acts
	// on). Zero attention (tests, a bare UI) shows no countdown.
	attentionCfg config.AttentionConfig
	quietHours   config.QuietHoursConfig
	// learningCfg shows the self-improvement budget pool on the Learning
	// page; zero hides the pool line. apiRunners scope the pool to API-billed
	// spend; usage reports subscription-window capacity (nil hides it).
	learningCfg config.LearningConfig
	apiRunners  []string
	usage       func(ctx context.Context) (engine.Usage, error)
}

// SetAppStatus wires the run supervisor's live app state into the Repos list:
// the per-row app chip and the live URL (preferred over the static app_url,
// which goes stale when a restart leases a different port). Nil hides both.
func (u *UI) SetAppStatus(fn func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)) {
	u.appStatus = fn
}

// SetPluginHealth wires the supervisor's live state into the System page; the
// daemon calls it once at startup.
func (u *UI) SetPluginHealth(fn func() []plugin.PluginHealth) { u.pluginHealth = fn }

// SetAttention wires the fuzzy Human Queue's SLA into the UI so the queue shows
// each non-critical question's countdown; the daemon calls it once at startup.
func (u *UI) SetAttention(cfg config.AttentionConfig, quiet config.QuietHoursConfig) {
	u.attentionCfg, u.quietHours = cfg, quiet
}

// SetLearning wires the [learning] budget into the Learning page's header;
// the daemon calls it once at startup. apiRunners are the API-billed runner
// names — the only spend the USD pool meters; usage (nil for a bare UI)
// reports the subscription windows so the page can show real capacity.
func (u *UI) SetLearning(cfg config.LearningConfig, apiRunners []string, usage func(ctx context.Context) (engine.Usage, error)) {
	u.learningCfg, u.apiRunners, u.usage = cfg, apiRunners, usage
}

// NewUI parses the embedded templates once; a template error is a startup error.
func NewUI(st *store.Store, log *slog.Logger, clock func() time.Time, promptsFn func() *directives.Library) (*UI, error) {
	if clock == nil {
		clock = time.Now
	}
	funcs := template.FuncMap{
		"short": func(s string) string {
			if len(s) > 8 {
				return s[:8]
			}
			return s
		},
		"ago": func(t time.Time) string {
			if t.IsZero() {
				return "-"
			}
			return humanDuration(clock().Sub(t)) + " ago"
		},
		// until renders a future instant as a remaining-time phrase (the Human
		// Queue's "Forge decides in …" countdown); a past or zero instant reads
		// "now".
		"until": func(t time.Time) string {
			if d := t.Sub(clock()); d > 0 {
				return "~" + humanDuration(d)
			}
			return "now"
		},
		"dur": func(us *int64) string {
			if us == nil {
				return "-"
			}
			return humanDuration(time.Duration(*us) * time.Microsecond)
		},
		"join": strings.Join,
		// causeLabel and causeFwd render a provenance cause (DESIGN.md §3) as a
		// backward ("planned by") and forward ("planned") phrase for the task
		// strip and the Work view.
		"causeLabel": func(c model.Cause) string {
			switch c {
			case model.CausePlanTask:
				return "planned by"
			case model.CauseVerify:
				return "verify of"
			case model.CauseFollowUp:
				return "follow-up of"
			}
			return "caused by"
		},
		"causeFwd": func(c model.Cause) string {
			switch c {
			case model.CausePlanTask:
				return "planned"
			case model.CauseVerify:
				return "verify"
			case model.CauseFollowUp:
				return "follow-up"
			}
			return ""
		},
		// dict builds the argument for a shared partial ({{template "searchbar" dict …}}).
		"dict": func(pairs ...any) (map[string]any, error) {
			if len(pairs)%2 != 0 {
				return nil, fmt.Errorf("dict: odd argument count")
			}
			m := make(map[string]any, len(pairs)/2)
			for i := 0; i < len(pairs); i += 2 {
				k, ok := pairs[i].(string)
				if !ok {
					return nil, fmt.Errorf("dict: key %v is not a string", pairs[i])
				}
				m[k] = pairs[i+1]
			}
			return m, nil
		},
		"trunc": func(s string) string {
			if r := []rune(s); len(r) > 60 {
				return string(r[:60]) + "…"
			}
			return s
		},
		"deref": func(p *float64) string {
			if p == nil {
				return "-"
			}
			return fmt.Sprintf("$%.4f", *p)
		},
		"stateClass": func(s any) string { return "state-" + strings.ReplaceAll(fmt.Sprint(s), "_", "-") },
		// appStateClass maps the run supervisor's app states onto the existing
		// state pill palette (style.css has no app-specific classes).
		"appStateClass": func(s string) string {
			switch s {
			case "running":
				return "state-running"
			case "errored":
				return "state-failed"
			case "building", "starting":
				return "state-preparing"
			}
			return "state-idle"
		},
		// stateIcon maps a state to the sprite symbol id for its pill glyph,
		// following the same groups as the .state-* colors in style.css; idle and
		// any unmapped state get "" (no glyph).
		"stateIcon": func(s any) string {
			switch strings.ReplaceAll(fmt.Sprint(s), "_", "-") {
			case "running", "claimed", "preparing", "verifying", "merging", "approved":
				return "i-st-running"
			case "waiting-human", "proposed":
				return "i-st-waiting"
			case "paused":
				return "i-st-paused"
			case "unverified":
				return "i-st-unverified"
			case "succeeded", "merged", "applied":
				return "i-st-ok"
			case "failed", "partial", "cancelled", "conflict", "errored", "rejected", "reverted":
				return "i-st-x"
			}
			return ""
		},
		// originURL turns a github.com/…-style origin identity into a browsable
		// https link; other forms (ssh remotes, bare paths) yield "" so the
		// template shows the identity as plain text.
		"originURL": func(origin string) string {
			for _, host := range []string{"github.com/", "gitlab.com/", "bitbucket.org/"} {
				if strings.HasPrefix(origin, host) {
					return "https://" + origin
				}
			}
			return ""
		},
		// refCommit extracts the commit sha from a fragment applied_ref
		// ("directive:x@<sha>"), "" for every other ref shape — the proposal
		// detail page links it to the diff view.
		"refCommit": func(ref string) string {
			if i := strings.LastIndex(ref, "@"); i > 0 && (strings.HasPrefix(ref, "directive:") || strings.HasPrefix(ref, "persona:")) {
				sha := ref[i+1:]
				if len(sha) >= 7 {
					return sha
				}
			}
			return ""
		},
		"mulf":       func(a, b float64) float64 { return a * b },
		"dereff":     func(p *float64) float64 { return *p },
		"deref2bool": func(p *bool) bool { return p != nil && *p },
		// prettyJSON indents a raw JSON value for the proposal detail page's
		// before/after and outcome blocks; malformed or empty yields the raw text.
		"prettyJSON": func(raw json.RawMessage) string {
			if len(raw) == 0 {
				return ""
			}
			var buf bytes.Buffer
			if err := json.Indent(&buf, raw, "", "  "); err != nil {
				return string(raw)
			}
			return buf.String()
		},
		"dur64": func(us int64) string {
			if us == 0 {
				return "-"
			}
			return humanDuration(time.Duration(us) * time.Microsecond)
		},
		// routing decodes an attempt's stored routing decision (M10) for the
		// task-detail page; nil (rendered as {{with}} skips) when absent or
		// unreadable, so a pre-M10 attempt shows nothing.
		"routing": func(raw json.RawMessage) *engine.RoutingDecision {
			if len(raw) == 0 {
				return nil
			}
			var d engine.RoutingDecision
			if json.Unmarshal(raw, &d) != nil {
				return nil
			}
			return &d
		},
	}
	tmpl, err := template.New("").Funcs(funcs).ParseFS(uiFS, "*.html")
	if err != nil {
		return nil, fmt.Errorf("parse ui templates: %w", err)
	}
	u := &UI{store: st, log: log, clock: clock, tmpl: tmpl, mux: http.NewServeMux(), prompts: promptsFn}
	static, err := fs.Sub(uiFS, "static")
	if err != nil {
		return nil, fmt.Errorf("ui static: %w", err)
	}
	// Embedded assets carry no modtime, so the FileServer emits no cache
	// validators and browsers cache heuristically — which serves stale JS
	// after a daemon upgrade. no-cache forces a revalidation-shaped refetch;
	// the assets are small and the daemon is local.
	staticFiles := http.StripPrefix("/static/", http.FileServer(http.FS(static)))
	u.mux.Handle("GET /static/", http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Cache-Control", "no-cache")
		staticFiles.ServeHTTP(w, r)
	}))
	u.mux.HandleFunc("GET /{$}", u.dashboard)
	u.mux.HandleFunc("GET /tasks", u.tasks)
	u.mux.HandleFunc("GET /tasks/rows", u.taskRowsFragment)
	u.mux.HandleFunc("GET /tasks/{id}", u.task)
	u.mux.HandleFunc("GET /work/{id}", u.work)
	u.mux.HandleFunc("GET /directives", u.directives)
	u.mux.HandleFunc("GET /routines", u.routinesPage)
	u.mux.HandleFunc("GET /workflows", u.workflows)
	u.mux.HandleFunc("GET /workflows/new", u.workflowEdit)
	u.mux.HandleFunc("GET /workflows/{name}/edit", u.workflowEdit)
	u.mux.HandleFunc("GET /workflows/{name}/runs", u.workflowRuns)
	u.mux.HandleFunc("GET /workflows/{name}/runs/{id}", u.workflowRun)
	u.mux.HandleFunc("GET /settings", u.settingsGeneral)
	u.mux.HandleFunc("GET /settings/plugins", u.settingsPlugins)
	u.mux.HandleFunc("GET /system", u.system)
	u.mux.HandleFunc("GET /repos", u.repos)
	u.mux.HandleFunc("GET /repos/{name}", u.repo)
	u.mux.HandleFunc("GET /queue", u.queue)
	u.mux.HandleFunc("GET /attention", u.attention)
	u.mux.HandleFunc("GET /proposals", u.proposals)
	u.mux.HandleFunc("GET /proposals/{id}", u.proposal)
	u.mux.HandleFunc("GET /kb", u.kb)
	u.mux.HandleFunc("GET /kb/{id}", u.kbNote)
	u.mux.HandleFunc("GET /stats", u.stats)
	u.mux.HandleFunc("GET /learning", u.learning)
	u.mux.HandleFunc("GET /learning/commits/{sha}", u.learningCommit)
	return u, nil
}

// Handler is the page mux.
func (u *UI) Handler() http.Handler { return u.mux }

func humanDuration(d time.Duration) string {
	switch {
	case d < time.Second:
		return fmt.Sprintf("%dms", d.Milliseconds())
	case d < time.Minute:
		return fmt.Sprintf("%.1fs", d.Seconds())
	case d < time.Hour:
		return fmt.Sprintf("%dm%02ds", int(d.Minutes()), int(d.Seconds())%60)
	case d < 48*time.Hour:
		return fmt.Sprintf("%dh%02dm", int(d.Hours()), int(d.Minutes())%60)
	}
	return fmt.Sprintf("%dd", int(d.Hours()/24))
}

type page struct {
	Title  string
	Active string
	Data   any
	Error  string
}

func (u *UI) render(w http.ResponseWriter, r *http.Request, name, title string, data any) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := u.tmpl.ExecuteTemplate(w, name, page{Title: title, Active: name, Data: data}); err != nil {
		u.log.ErrorContext(r.Context(), "render page", "page", name, "error", err)
	}
}

func (u *UI) fail(w http.ResponseWriter, r *http.Request, err error) {
	u.log.ErrorContext(r.Context(), "ui", "path", r.URL.Path, "error", err)
	http.Error(w, err.Error(), http.StatusInternalServerError)
}

// renderPartial writes one named template with no page layout — used for the
// HTML fragments the JS loaders append (see taskRowsFragment).
func (u *UI) renderPartial(w http.ResponseWriter, r *http.Request, name string, data any) {
	if err := u.tmpl.ExecuteTemplate(w, name, data); err != nil {
		u.log.ErrorContext(r.Context(), "render partial", "template", name, "error", err)
	}
}

// taskRow is one Work with its derived state for lists.
type taskRow struct {
	Work    store.Work
	State   model.WorkState
	Targets []store.Target
}

func (u *UI) taskRows(ctx context.Context, works []store.Work) ([]taskRow, error) {
	ids := make([]string, len(works))
	for i, w := range works {
		ids[i] = w.ID
	}
	targets, err := u.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, err
	}
	rows := make([]taskRow, 0, len(works))
	for _, w := range works {
		ts := targets[w.ID]
		rows = append(rows, taskRow{Work: w, State: model.DeriveWorkState(model.WorkInputs{Targets: engine.TargetStates(ts), Integrate: w.Integrate}), Targets: ts})
	}
	return rows, nil
}

func (u *UI) dashboard(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	works, err := u.store.ListWork(ctx, 20)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	rows, err := u.taskRows(ctx, works)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	workers, err := u.store.Workers(ctx, u.clock())
	if err != nil {
		u.fail(w, r, err)
		return
	}
	questions, err := u.store.OpenQuestions(ctx)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	proposals, err := u.store.ListProposals(ctx, model.ProposalProposed)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	var running, attention []taskRow
	for _, row := range rows {
		switch row.State {
		case model.WorkRunning, model.WorkMerging:
			running = append(running, row)
		case model.WorkFailed, model.WorkPartial, model.WorkUnverified, model.WorkWaitingHuman, model.WorkConflict:
			attention = append(attention, row)
		}
	}
	runners := runnerHealth(workers)
	pending := 0
	for _, row := range rows {
		for _, t := range row.Targets {
			if t.State == model.Pending {
				pending++
			}
		}
	}
	repos, err := u.repoChips(ctx)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "dashboard.html", "Dashboard", map[string]any{"Recent": rows, "Running": running, "Attention": attention, "Workers": workers, "Questions": questions, "Proposals": proposals, "Runners": runners, "QueueDepth": pending, "Repositories": repos})
}

// repoChip is one launcher entry on the dashboard's repositories strip: the
// name, its state, and where the chip links (the running app when set and the
// repository is running, else its detail page).
type repoChip struct {
	Name   string
	State  string
	AppURL string
	Href   string
}

func (u *UI) repoChips(ctx context.Context) ([]repoChip, error) {
	repos, err := u.store.Repositories(ctx)
	if err != nil {
		return nil, err
	}
	states, err := web.RepositoryStates(ctx, u.store, u.clock())
	if err != nil {
		return nil, err
	}
	out := make([]repoChip, 0, len(repos))
	for _, r := range repos {
		st := states[r.Name]
		href := "/repos/" + r.Name
		if st == "running" && r.AppURL != "" {
			href = r.AppURL
		}
		out = append(out, repoChip{Name: r.Name, State: st, AppURL: r.AppURL, Href: href})
	}
	return out, nil
}

// repo is a repository's main page: state, controls (pause/resume, cancel
// running, set app url), the local path and origin, declared checks, running and
// recent tasks, and the retained-worktree cleanup hint.
func (u *UI) repo(w http.ResponseWriter, r *http.Request) {
	name := r.PathValue("name")
	detail, err := web.BuildRepoDetail(r.Context(), u.store, u.clock(), name)
	if errors.Is(err, store.ErrNotFound) {
		http.NotFound(w, r)
		return
	}
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "repo.html", "Repository "+name, detail)
}

// tasksPageSize is how many tasks the Tasks page loads per request — the first
// page server-rendered, each next page appended by the infinite-scroll loader.
const tasksPageSize = 100

// tasksData backs tasks.html: the current scope, its first page of rows, whether
// a next page exists, and the repository names for the New-task datalist.
type tasksData struct {
	Scope        string
	Rows         []taskRow
	HasMore      bool
	Repositories []string
}

// taskScope clamps the ?scope= param to open (the default — unfinished work),
// closed (terminal), or all.
func taskScope(q string) string {
	switch q {
	case "closed", "all":
		return q
	default:
		return "open"
	}
}

func (u *UI) tasks(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	scope := taskScope(r.URL.Query().Get("scope"))
	rows, hasMore, err := u.taskPage(ctx, scope, 0)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	repos, err := u.store.Repositories(ctx)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	names := make([]string, len(repos))
	for i, rep := range repos {
		names[i] = rep.Name
	}
	u.render(w, r, "tasks.html", "Tasks", tasksData{Scope: scope, Rows: rows, HasMore: hasMore, Repositories: names})
}

// taskRowsFragment serves one appended page of task rows (bare <tr>s) for the
// infinite-scroll loader: GET /tasks/rows?scope=&offset=. An empty body means
// no more rows; the loader stops on that.
func (u *UI) taskRowsFragment(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	scope := taskScope(r.URL.Query().Get("scope"))
	offset := 0
	if n, err := strconv.Atoi(r.URL.Query().Get("offset")); err == nil {
		offset = n
	}
	rows, hasMore, err := u.taskPage(ctx, scope, offset)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if hasMore {
		w.Header().Set("X-Has-More", "1")
	}
	u.renderPartial(w, r, "taskRowList", rows)
}

// taskPage reads one page of Work for scope at offset and derives its rows,
// asking for one extra row to tell whether a further page exists.
func (u *UI) taskPage(ctx context.Context, scope string, offset int) ([]taskRow, bool, error) {
	works, err := u.store.ListWorkPage(ctx, scope, tasksPageSize+1, offset)
	if err != nil {
		return nil, false, err
	}
	hasMore := len(works) > tasksPageSize
	if hasMore {
		works = works[:tasksPageSize]
	}
	rows, err := u.taskRows(ctx, works)
	return rows, hasMore, err
}

func (u *UI) task(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	work, err := u.store.GetWork(ctx, r.PathValue("id"))
	if err != nil {
		http.NotFound(w, r)
		return
	}
	targets, err := u.store.TargetsForWork(ctx, work.ID)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	ids := make([]string, len(targets))
	for i, t := range targets {
		ids[i] = t.ID
	}
	attempts, err := u.store.AttemptsForTargets(ctx, ids)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	questions, err := u.store.QuestionsForWork(ctx, work.ID)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	type targetView struct {
		Target   store.Target
		Attempts []store.Attempt
		Events   []store.StoredEvent
		Facts    *store.AttemptFacts
	}
	views := make([]targetView, 0, len(targets))
	for _, t := range targets {
		tv := targetView{Target: t, Attempts: attempts[t.ID]}
		// Attach the live progress tally to each unfinished attempt so the page
		// can show a running attempt's phase, last-event age, and turns/tokens.
		for i := range tv.Attempts {
			if !tv.Attempts[i].FinishedAt.IsZero() {
				if p, err := u.store.AttemptProgress(ctx, tv.Attempts[i].ID); err == nil {
					tv.Attempts[i].Progress = p
				}
			}
		}
		if n := len(tv.Attempts); n > 0 {
			last := tv.Attempts[n-1]
			if evs, err := u.store.Events(ctx, last.ID, false, 200); err == nil {
				tv.Events = evs
			}
			if f, err := u.store.FactsForAttempt(ctx, last.ID); err == nil {
				tv.Facts = f
			}
		}
		views = append(views, tv)
	}
	state := model.DeriveWorkState(model.WorkInputs{Targets: engine.TargetStates(targets), Integrate: work.Integrate})
	// Provenance strip (DESIGN.md §3): one lineage lookup feeds the breadcrumb,
	// the backward cause/deps, and the forward children/blocked links.
	var strip provStrip
	if ld, err := web.ComputeLineage(ctx, u.store, work.ID); err == nil {
		strip = lineageStrip(ld, work.ID)
	}
	u.render(w, r, "task.html", "Task "+work.ID[:8], map[string]any{"Work": work, "State": state, "Targets": views, "Questions": questions, "Lineage": strip})
}

// work is the /work/{id} view: one provenance tree with the root's rollups. Any
// member id canonicalizes (302) to the root's URL.
func (u *UI) work(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	ld, err := web.ComputeLineage(ctx, u.store, r.PathValue("id"))
	if err != nil {
		http.NotFound(w, r)
		return
	}
	if r.PathValue("id") != ld.RootID {
		http.Redirect(w, r, "/work/"+ld.RootID, http.StatusFound)
		return
	}
	var targetIDs []string
	for _, ts := range ld.Targets {
		for _, t := range ts {
			targetIDs = append(targetIDs, t.ID)
		}
	}
	attempts, err := u.store.AttemptsForTargets(ctx, targetIDs)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	tree, roll := lineageBuildTree(ld, attempts)
	root := ld.ByID[ld.RootID]
	u.render(w, r, "work.html", "Work "+ld.RootID[:8], map[string]any{"Root": root, "State": ld.State[ld.RootID], "Tree": tree, "Rollup": roll})
}

// promptTreeItem is one row of the Prompts page's library tree.
type promptTreeItem struct {
	Name      string // full fragment name (may contain /)
	Label     string // last path segment, indented under its folder
	Folder    string // "" for top-level files
	Persona   bool
	Directive bool
}

// directives is the library page: the file-backed tree (personas/,
// directives/, scripts/, fragments/). Details, composition previews, and
// testing are client-side against the API (static/directives.js); this
// handler only shapes the tree.
func (u *UI) directives(w http.ResponseWriter, r *http.Request) {
	repos, err := u.store.Repositories(r.Context())
	if err != nil {
		u.fail(w, r, err)
		return
	}
	names := make([]string, len(repos))
	for i, rep := range repos {
		names[i] = rep.Name
	}
	var personas, fragments, directives, scripts []promptTreeItem
	libDir := ""
	if u.prompts != nil {
		if lib := u.prompts(); lib != nil {
			libDir = lib.Dir
			for _, f := range lib.Fragments() {
				item := promptTreeItem{Name: f.Name, Label: f.Name, Persona: f.Persona, Directive: f.Directive}
				if i := strings.LastIndex(f.Name, "/"); i >= 0 {
					item.Folder, item.Label = f.Name[:i], f.Name[i+1:]
				}
				switch {
				case f.Persona:
					personas = append(personas, item)
				case f.Directive:
					directives = append(directives, item)
				default:
					fragments = append(fragments, item)
				}
			}
		}
	}
	scratch, err := u.store.ListScratch(r.Context())
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "directives.html", "Directives", map[string]any{
		"Repositories": names,
		"Personas":     personas, "Fragments": fragments, "Directives": directives, "Scripts": scripts,
		"Scratch": scratch, "LibDir": libDir,
	})
}

// routinesPage lists the trigger routines: what fires, when, at which
// target. Content lives with the target; the dialog edits the trigger.
func (u *UI) routinesPage(w http.ResponseWriter, r *http.Request) {
	rs, err := u.store.ListRoutines(r.Context(), false)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	repos, err := u.store.Repositories(r.Context())
	if err != nil {
		u.fail(w, r, err)
		return
	}
	repoNames := make([]string, len(repos))
	for i, rep := range repos {
		repoNames[i] = rep.Name
	}
	type rtRow struct {
		store.Routine
		TargetLink string
	}
	rows := make([]rtRow, 0, len(rs))
	for _, rt := range rs {
		row := rtRow{Routine: rt}
		if kind, name, err := store.ParseTarget(rt.Target); err == nil {
			switch kind {
			case store.TargetDirective, store.TargetScript:
				row.TargetLink = "/directives?sel=prompt:" + name
			case store.TargetWorkflow:
				row.TargetLink = "/workflows/" + name + "/edit"
			}
		}
		rows = append(rows, row)
	}
	var directiveNames, scriptNames []string
	if u.prompts != nil {
		if lib := u.prompts(); lib != nil {
			for _, f := range lib.Fragments() {
				switch {
				case f.Directive:
					directiveNames = append(directiveNames, f.Name)
				case f.Script:
					scriptNames = append(scriptNames, f.Name)
				}
			}
		}
	}
	wfs, err := u.store.ListWorkflows(r.Context(), false)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	wfNames := make([]string, len(wfs))
	for i, wf := range wfs {
		wfNames[i] = wf.Name
	}
	u.render(w, r, "routines.html", "Routines", map[string]any{
		"Routines": rows, "Repositories": repoNames,
		"Directives": directiveNames, "ScriptNames": scriptNames, "Workflows": wfNames,
	})
}

// workflows lists workflows with the add/edit dialog; routine names feed the
// step editor's datalist. The API does the writing — app.js posts and reloads.
func (u *UI) workflows(w http.ResponseWriter, r *http.Request) {
	wfs, err := u.store.ListWorkflows(r.Context(), false)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	rs, err := u.store.ListRoutines(r.Context(), false)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	names := make([]string, len(rs))
	for i, rt := range rs {
		names[i] = rt.Name
	}
	type wfRow struct {
		store.Workflow
		StepNames []string
	}
	rows := make([]wfRow, 0, len(wfs))
	for _, wf := range wfs {
		row := wfRow{Workflow: wf}
		if wf.Graph != nil {
			row.StepNames = make([]string, len(wf.Graph.Nodes))
			for i, n := range wf.Graph.Nodes {
				row.StepNames[i] = n.ID
			}
		}
		rows = append(rows, row)
	}
	u.render(w, r, "workflows.html", "Workflows", map[string]any{"Workflows": rows, "Routines": names})
}

// workflowEdit is the graph editor shell for /workflows/new and
// /workflows/{name}/edit. The client fetches the workflow (and routine names)
// from the API; the page only carries which workflow to load.
func (u *UI) workflowEdit(w http.ResponseWriter, r *http.Request) {
	name := r.PathValue("name")
	title := "New workflow"
	if name != "" {
		title = "Edit " + name
	}
	u.render(w, r, "workflow-edit.html", title, map[string]any{"Name": name})
}

// workflowRuns lists a workflow's engine runs, newest first. Runs from before
// the run engine have no rows here; the task search link below the table
// still finds their Works by title prefix.
func (u *UI) workflowRuns(w http.ResponseWriter, r *http.Request) {
	name := r.PathValue("name")
	wf, err := u.store.GetWorkflow(r.Context(), name)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	runs, err := u.store.WorkflowRunsFor(r.Context(), name, 100)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	type runRow struct {
		store.WorkflowRun
		Nodes []store.RunNode
	}
	rows := make([]runRow, 0, len(runs))
	for _, run := range runs {
		nodes, err := u.store.RunNodes(r.Context(), run.ID)
		if err != nil {
			u.fail(w, r, err)
			return
		}
		rows = append(rows, runRow{WorkflowRun: run, Nodes: nodes})
	}
	u.render(w, r, "workflow-runs.html", wf.Name+" runs", map[string]any{"Workflow": wf, "Runs": rows})
}

// workflowRun is the run view shell: the frozen graph rendered read-only with
// live per-node states. The client fetches the run detail from the API.
func (u *UI) workflowRun(w http.ResponseWriter, r *http.Request) {
	name, id := r.PathValue("name"), r.PathValue("id")
	u.render(w, r, "workflow-run.html", name+" run", map[string]any{"Name": name, "RunID": id})
}

// repos is the Repos page: every registered repository (archived included),
// with add / pause / archive / restore controls.
func (u *UI) repos(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	repos, err := u.store.Repositories(ctx)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	states, err := web.RepositoryStates(ctx, u.store, u.clock())
	if err != nil {
		u.fail(w, r, err)
		return
	}
	// Live app state per row (P2, TODO 2026-09-01): the task-health chip says
	// nothing about a running app, and the static app_url goes stale when a
	// restart leases a new port — prefer the supervisor's live URL.
	apps := map[string]protocol.AppStatus{}
	if u.appStatus != nil {
		for _, repo := range repos {
			if repo.Archived {
				continue
			}
			st, err := u.appStatus(ctx, repo.Name, repo.Path)
			if err != nil {
				u.log.WarnContext(ctx, "app status for repos list", "repo", repo.Name, "error", err)
				continue
			}
			if st.Configured {
				apps[repo.Name] = st
			}
		}
	}
	u.render(w, r, "repos.html", "Repos", map[string]any{"Repositories": repos, "RepoStates": states, "Apps": apps})
}

func (u *UI) system(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	workers, err := u.store.Workers(ctx, u.clock())
	if err != nil {
		u.fail(w, r, err)
		return
	}
	repos, err := u.store.Repositories(ctx)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	plugins, err := u.store.Plugins(ctx)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	health := map[string]plugin.PluginHealth{}
	if u.pluginHealth != nil {
		for _, h := range u.pluginHealth() {
			health[h.Name] = h
		}
	}
	rows := make([]uiPlugin, 0, len(plugins))
	for _, p := range plugins {
		h := health[p.Name]
		rows = append(rows, uiPlugin{Plugin: p, Running: h.Running, PID: h.PID, Restarts: h.Restarts, LastExit: h.LastExit})
	}
	states, err := web.RepositoryStates(ctx, u.store, u.clock())
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "system.html", "System", map[string]any{"Workers": workers, "Repositories": repos, "Plugins": rows, "RepoStates": states})
}

// settingsGeneral is the Settings hub landing page. The daemon health panel and
// the log-level control are filled and driven client-side (GET /api/v1/health,
// GET|POST /api/v1/log-level), so the handler is a thin shell.
func (u *UI) settingsGeneral(w http.ResponseWriter, r *http.Request) {
	u.render(w, r, "settings.html", "Settings", nil)
}

// settingsPlugins is the dedicated plugin-management page: the installed
// plugins with their health, enable/disable/uninstall controls, and an install
// form. Same plugin gathering as the System page.
func (u *UI) settingsPlugins(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	plugins, err := u.store.Plugins(ctx)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	health := map[string]plugin.PluginHealth{}
	if u.pluginHealth != nil {
		for _, h := range u.pluginHealth() {
			health[h.Name] = h
		}
	}
	rows := make([]uiPlugin, 0, len(plugins))
	for _, p := range plugins {
		h := health[p.Name]
		rows = append(rows, uiPlugin{Plugin: p, Running: h.Running, PID: h.PID, Restarts: h.Restarts, LastExit: h.LastExit})
	}
	u.render(w, r, "settings-plugins.html", "Settings", map[string]any{"Plugins": rows})
}

// uiPlugin is one System-page plugin row: the store row plus live health.
type uiPlugin struct {
	store.Plugin
	Running  bool
	PID      int
	Restarts int
	LastExit string
}

// stats renders the same report the API and CLI serve (stats.Load is the one
// aggregation), for the window in ?since (default 7d).
func (u *UI) stats(w http.ResponseWriter, r *http.Request) {
	since := r.URL.Query().Get("since")
	if since == "" {
		since = "7d"
	}
	dur, err := web.ParseSince(since)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	now := u.clock()
	report, err := stats.Load(r.Context(), u.store, stats.Query{Since: now.Add(-dur), Until: now})
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "stats.html", "Stats", map[string]any{"Report": report, "Since": since})
}

// queueRows loads the queue in the one true order (controlplane/queue.Order),
// without budget deferral (the UI shows a "deferred" state only when the API
// exposes it; M3's budget policy feeds the API, and this page mirrors it).
func (u *UI) queueRows(ctx context.Context) ([]engine.QueueEntry, error) {
	works, err := u.store.OpenWork(ctx)
	if err != nil {
		return nil, err
	}
	ids := make([]string, len(works))
	for i, w := range works {
		ids[i] = w.ID
	}
	targets, err := u.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, err
	}
	edges, err := u.store.DependencyEdges(ctx)
	if err != nil {
		return nil, err
	}
	return engine.Order(engine.QueueInput{Work: works, Targets: targets, Edges: edges}), nil
}

// queue is the drag-and-drop priority page.
func (u *UI) queue(w http.ResponseWriter, r *http.Request) {
	rows, err := u.queueRows(r.Context())
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "queue.html", "Queue", rows)
}

func (u *UI) attention(w http.ResponseWriter, r *http.Request) {
	questions, err := u.store.OpenQuestions(r.Context())
	if err != nil {
		u.fail(w, r, err)
		return
	}
	type row struct {
		Question store.Question
		Work     *store.Work
		Actions  []QueueAction
		// AutoDecide is true when Forge will decide this question once its
		// Deadline lapses (non-critical, auto-decision on); critical questions
		// have AutoDecide false and no Deadline — they need a human.
		AutoDecide bool
		Deadline   time.Time
	}
	rows := make([]row, 0, len(questions))
	now := u.clock()
	for _, q := range questions {
		work, err := u.store.GetWork(r.Context(), q.WorkID)
		if err != nil {
			u.fail(w, r, err)
			return
		}
		deadline, auto := engine.AttentionDeadline(q, now, u.attentionCfg, u.quietHours)
		rows = append(rows, row{Question: q, Work: work, Actions: questionActions(q.Context), AutoDecide: auto, Deadline: deadline})
	}
	// Most urgent first: auto-deciding questions by soonest deadline, then the
	// critical "needs you" items (which never auto-decide) by age.
	sort.SliceStable(rows, func(i, j int) bool {
		if rows[i].AutoDecide != rows[j].AutoDecide {
			return rows[i].AutoDecide
		}
		if rows[i].AutoDecide {
			return rows[i].Deadline.Before(rows[j].Deadline)
		}
		return rows[i].Question.AskedAt.Before(rows[j].Question.AskedAt)
	})
	proposals, err := u.store.ListProposals(r.Context(), model.ProposalProposed)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	// proposalRow adds the "open the thing" link a card renders when the
	// proposal's target has a page (a kb-note doc).
	type proposalRow struct {
		store.Proposal
		OpenURL string
	}
	prows := make([]proposalRow, 0, len(proposals))
	for _, p := range proposals {
		prows = append(prows, proposalRow{Proposal: p, OpenURL: proposalOpenURL(p)})
	}
	u.render(w, r, "attention.html", "Human Queue", map[string]any{"Questions": rows, "Proposals": prows})
}

// proposals lists every proposal with the decision buttons; the API does the
// writing (POST approve applies in the same transaction), app.js only posts
// and reloads.
// proposal is the proposal detail page: the full record, its before/after and
// A/B outcome, and the decision history from the journal.
func (u *UI) proposal(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	id := r.PathValue("id")
	p, err := u.store.GetProposal(ctx, id)
	if errors.Is(err, store.ErrNotFound) {
		http.NotFound(w, r)
		return
	}
	if err != nil {
		u.fail(w, r, err)
		return
	}
	predictions, err := u.store.PredictionsForProposal(ctx, id)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	history, err := u.store.JournalForEntity(ctx, store.EntityProposal, id)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	title := "Proposal " + id
	if len(id) > 8 {
		title = "Proposal " + id[:8]
	}
	u.render(w, r, "proposal-detail.html", title, map[string]any{"Proposal": p, "History": history, "Predictions": predictions})
}

// proposalsData backs proposals.html: the current scope, the proposals in it,
// and the per-scope counts the tabs show.
type proposalsData struct {
	Scope     string
	Proposals []store.Proposal
	Open      int
	Closed    int
}

// proposalScope clamps the ?scope= param to open (the default — proposals
// still waiting on a decision or an apply), closed (decided and done), or all.
func proposalScope(q string) string {
	switch q {
	case "closed", "all":
		return q
	default:
		return "open"
	}
}

// proposalOpen reports whether a proposal still has somewhere to go: undecided,
// or approved but not yet applied. Rejected, applied and reverted are closed.
func proposalOpen(s model.ProposalStatus) bool {
	return s == model.ProposalProposed || s == model.ProposalApproved
}

func (u *UI) proposals(w http.ResponseWriter, r *http.Request) {
	all, err := u.store.ListProposals(r.Context(), "")
	if err != nil {
		u.fail(w, r, err)
		return
	}
	data := proposalsData{Scope: proposalScope(r.URL.Query().Get("scope")), Proposals: []store.Proposal{}}
	for _, p := range all {
		open := proposalOpen(p.Status)
		if open {
			data.Open++
		} else {
			data.Closed++
		}
		if data.Scope == "all" || open == (data.Scope == "open") {
			data.Proposals = append(data.Proposals, p)
		}
	}
	u.render(w, r, "proposals.html", "Proposals", data)
}

// runnerHealthRow is one runner's advertised health for the dashboard (M10).
type runnerHealthRow struct {
	Name  string
	State string
}

// runnerHealth aggregates the runner:<name> capabilities advertised across all
// workers into one health row per runner: the best state any worker reports
// wins (a runner is available while any worker can run it). Sorted by name.
func runnerHealth(workers []store.Worker) []runnerHealthRow {
	rank := map[string]int{"ready": 3, "unauthenticated": 2, "down": 1}
	best := map[string]string{}
	for _, w := range workers {
		for k, v := range w.Capabilities {
			name, ok := strings.CutPrefix(k, "runner:")
			if !ok {
				continue
			}
			if cur, seen := best[name]; !seen || rank[v] > rank[cur] {
				best[name] = v
			}
		}
	}
	out := make([]runnerHealthRow, 0, len(best))
	for name, state := range best {
		out = append(out, runnerHealthRow{Name: name, State: state})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}
