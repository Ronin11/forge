package controlplane

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

	"forge/internal/model"
	"forge/internal/plugin"
	"forge/internal/stats"
	"forge/internal/store"
)

//go:embed ui/*.html ui/static/*
var uiFS embed.FS

// UI serves the operator pages: html/template over the store, one stylesheet,
// vanilla JS. It is mounted beside the API on the same listeners.
type UI struct {
	store *store.Store
	log   *slog.Logger
	clock func() time.Time
	tmpl  *template.Template
	mux   *http.ServeMux
	// pluginHealth is the supervisor's live view for the System page; nil
	// (tests, a UI without a daemon) renders installed rows as not running.
	pluginHealth func() []plugin.PluginHealth
	// attention + quietHours let the Human Queue show each non-critical
	// question's countdown to auto-decision (the same deadline the sweep acts
	// on). Zero attention (tests, a bare UI) shows no countdown.
	attentionCfg AttentionConfig
	quietHours   QuietHoursConfig
}

// SetPluginHealth wires the supervisor's live state into the System page; the
// daemon calls it once at startup.
func (u *UI) SetPluginHealth(fn func() []plugin.PluginHealth) { u.pluginHealth = fn }

// SetAttention wires the fuzzy Human Queue's SLA into the UI so the queue shows
// each non-critical question's countdown; the daemon calls it once at startup.
func (u *UI) SetAttention(cfg AttentionConfig, quiet QuietHoursConfig) {
	u.attentionCfg, u.quietHours = cfg, quiet
}

// NewUI parses the embedded templates once; a template error is a startup error.
func NewUI(st *store.Store, log *slog.Logger, clock func() time.Time) (*UI, error) {
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
		"mulf":   func(a, b float64) float64 { return a * b },
		"dereff": func(p *float64) float64 { return *p },
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
		"routing": func(raw json.RawMessage) *RoutingDecision {
			if len(raw) == 0 {
				return nil
			}
			var d RoutingDecision
			if json.Unmarshal(raw, &d) != nil {
				return nil
			}
			return &d
		},
	}
	tmpl, err := template.New("").Funcs(funcs).ParseFS(uiFS, "ui/*.html")
	if err != nil {
		return nil, fmt.Errorf("parse ui templates: %w", err)
	}
	u := &UI{store: st, log: log, clock: clock, tmpl: tmpl, mux: http.NewServeMux()}
	static, err := fs.Sub(uiFS, "ui/static")
	if err != nil {
		return nil, fmt.Errorf("ui static: %w", err)
	}
	u.mux.Handle("GET /static/", http.StripPrefix("/static/", http.FileServer(http.FS(static))))
	u.mux.HandleFunc("GET /{$}", u.dashboard)
	u.mux.HandleFunc("GET /tasks", u.tasks)
	u.mux.HandleFunc("GET /tasks/rows", u.taskRowsFragment)
	u.mux.HandleFunc("GET /tasks/{id}", u.task)
	u.mux.HandleFunc("GET /work/{id}", u.work)
	u.mux.HandleFunc("GET /routines", u.routines)
	u.mux.HandleFunc("GET /workflows", u.workflows)
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
	return u, nil
}

// Handler is the page mux.
func (u *UI) Handler() http.Handler { return u.mux }

// MountUI serves the pages from the API server's mux; API routes are more
// specific than "/" so they keep winning.
func (s *Server) MountUI(u *UI) { s.mux.Handle("/", u.Handler()) }

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
		rows = append(rows, taskRow{Work: w, State: model.DeriveWorkState(model.WorkInputs{Targets: targetStates(ts), Integrate: w.Integrate}), Targets: ts})
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
	states, err := repositoryStates(ctx, u.store, u.clock())
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
	detail, err := buildRepoDetail(r.Context(), u.store, u.clock(), name)
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
	state := model.DeriveWorkState(model.WorkInputs{Targets: targetStates(targets), Integrate: work.Integrate})
	// Provenance strip (DESIGN.md §3): one lineage lookup feeds the breadcrumb,
	// the backward cause/deps, and the forward children/blocked links.
	var strip provStrip
	if ld, err := computeLineage(ctx, u.store, work.ID); err == nil {
		strip = ld.strip(work.ID)
	}
	u.render(w, r, "task.html", "Task "+work.ID[:8], map[string]any{"Work": work, "State": state, "Targets": views, "Questions": questions, "Lineage": strip})
}

// work is the /work/{id} view: one provenance tree with the root's rollups. Any
// member id canonicalizes (302) to the root's URL.
func (u *UI) work(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	ld, err := computeLineage(ctx, u.store, r.PathValue("id"))
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
	tree, roll := ld.buildTree(attempts)
	root := ld.byID[ld.RootID]
	u.render(w, r, "work.html", "Work "+ld.RootID[:8], map[string]any{"Root": root, "State": ld.State[ld.RootID], "Tree": tree, "Rollup": roll})
}

func (u *UI) routines(w http.ResponseWriter, r *http.Request) {
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
	names := make([]string, len(repos))
	for i, rep := range repos {
		names[i] = rep.Name
	}
	u.render(w, r, "routines.html", "Routines", map[string]any{"Routines": rs, "Repositories": names})
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
		row := wfRow{Workflow: wf, StepNames: make([]string, len(wf.Steps))}
		for i, st := range wf.Steps {
			row.StepNames[i] = st.Name
		}
		rows = append(rows, row)
	}
	u.render(w, r, "workflows.html", "Workflows", map[string]any{"Workflows": rows, "Routines": names})
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
	states, err := repositoryStates(ctx, u.store, u.clock())
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "repos.html", "Repos", map[string]any{"Repositories": repos, "RepoStates": states})
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
	states, err := repositoryStates(ctx, u.store, u.clock())
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
	dur, err := parseSince(since)
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
func (u *UI) queueRows(ctx context.Context) ([]QueueEntry, error) {
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
	return Order(QueueInput{Work: works, Targets: targets, Edges: edges}), nil
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
		deadline, auto := attentionDeadline(q, now, u.attentionCfg, u.quietHours)
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
	history, err := u.store.JournalForEntity(ctx, store.EntityProposal, id)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	title := "Proposal " + id
	if len(id) > 8 {
		title = "Proposal " + id[:8]
	}
	u.render(w, r, "proposal-detail.html", title, map[string]any{"Proposal": p, "History": history})
}

func (u *UI) proposals(w http.ResponseWriter, r *http.Request) {
	ps, err := u.store.ListProposals(r.Context(), "")
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "proposals.html", "Proposals", ps)
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
