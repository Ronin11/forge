package controlplane

import (
	"context"
	"embed"
	"fmt"
	"html/template"
	"io/fs"
	"log/slog"
	"net/http"
	"strings"
	"time"

	"forge/internal/model"
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
		"dur": func(us *int64) string {
			if us == nil {
				return "-"
			}
			return humanDuration(time.Duration(*us) * time.Microsecond)
		},
		"join": strings.Join,
		"deref": func(p *float64) string {
			if p == nil {
				return "-"
			}
			return fmt.Sprintf("$%.4f", *p)
		},
		"stateClass": func(s any) string { return "state-" + strings.ReplaceAll(fmt.Sprint(s), "_", "-") },
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
	u.mux.HandleFunc("GET /tasks/{id}", u.task)
	u.mux.HandleFunc("GET /routines", u.routines)
	u.mux.HandleFunc("GET /system", u.system)
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
	var running, attention []taskRow
	for _, row := range rows {
		switch row.State {
		case model.WorkRunning, model.WorkMerging:
			running = append(running, row)
		case model.WorkFailed, model.WorkPartial, model.WorkUnverified, model.WorkWaitingHuman, model.WorkConflict:
			attention = append(attention, row)
		}
	}
	u.render(w, r, "dashboard.html", "Dashboard", map[string]any{"Recent": rows, "Running": running, "Attention": attention, "Workers": workers, "Questions": questions})
}

func (u *UI) tasks(w http.ResponseWriter, r *http.Request) {
	works, err := u.store.ListWork(r.Context(), 200)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	rows, err := u.taskRows(r.Context(), works)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "tasks.html", "Tasks", rows)
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
	u.render(w, r, "task.html", "Task "+work.ID[:8], map[string]any{"Work": work, "State": state, "Targets": views, "Questions": questions})
}

func (u *UI) routines(w http.ResponseWriter, r *http.Request) {
	rs, err := u.store.ListRoutines(r.Context(), false)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "routines.html", "Routines", rs)
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
	u.render(w, r, "system.html", "System", map[string]any{"Workers": workers, "Repositories": repos})
}
