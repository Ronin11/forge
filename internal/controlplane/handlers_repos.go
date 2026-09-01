package controlplane

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"sort"
	"strings"
	"time"

	"forge/internal/core/model"
	"forge/internal/protocol"
	"forge/internal/store"
)

// repoRoutes serves the repository-controls surface (M12+): a per-repository
// detail read, pause/resume, cancel-running, and the running-app URL. They are
// operator routes (both listeners), mapped by fail() like the other writes.
func (s *Server) repoRoutes(m *http.ServeMux) {
	m.HandleFunc("POST /api/v1/repositories", s.handle(s.addRepository))
	m.HandleFunc("GET /api/v1/repositories/{name}", s.handle(s.getRepository))
	m.HandleFunc("POST /api/v1/repositories/{name}/archive", s.handle(s.archiveRepository))
	m.HandleFunc("POST /api/v1/repositories/{name}/restore", s.handle(s.restoreRepository))
	m.HandleFunc("GET /api/v1/repositories/{name}/app", s.handle(s.appStatusHandler))
	m.HandleFunc("POST /api/v1/repositories/{name}/app/start", s.handle(s.appStart))
	m.HandleFunc("POST /api/v1/repositories/{name}/app/stop", s.handle(s.appStop))
	m.HandleFunc("POST /api/v1/repositories/{name}/app/rebuild", s.handle(s.appRebuild))
	m.HandleFunc("POST /api/v1/repositories/{name}/pause", s.handle(s.pauseRepository))
	m.HandleFunc("POST /api/v1/repositories/{name}/resume", s.handle(s.resumeRepository))
	m.HandleFunc("POST /api/v1/repositories/{name}/cancel-running", s.handle(s.cancelRepositoryRunning))
	m.HandleFunc("POST /api/v1/repositories/{name}/app-url", s.handle(s.setRepositoryAppURL))
}

// repoState is the one home for a repository's health, used by the System page,
// the dashboard strip, the list endpoint, and the detail page. Precedence:
// paused (the flag) wins; else errored when the repository is not advertised by
// a connected worker or its most recent task ended failed/unverified within the
// last hour; else running when a target of it is active; else idle.
func repoState(paused, connected bool, targets []store.Target, now time.Time) string {
	if paused {
		return "paused"
	}
	active := false
	var recent *store.Target
	for i := range targets {
		if activeTargetState(targets[i].State) {
			active = true
		}
		if fin := targets[i].FinishedAt; !fin.IsZero() {
			if recent == nil || fin.After(recent.FinishedAt) {
				recent = &targets[i]
			}
		}
	}
	if !connected {
		return "errored"
	}
	if recent != nil && now.Sub(recent.FinishedAt) < time.Hour && (recent.State == model.Failed || recent.State == model.Unverified) {
		return "errored"
	}
	if active {
		return "running"
	}
	return "idle"
}

// activeTargetState reports the leased-or-verifying-or-merging states that count
// a repository as running.
func activeTargetState(s model.State) bool {
	switch s {
	case model.Claimed, model.Preparing, model.Running, model.Verifying, model.Merging:
		return true
	}
	return false
}

// repositoryStates derives every repository's state in one pass; the list
// endpoint and the System page share it so they never disagree.
func repositoryStates(ctx context.Context, st *store.Store, now time.Time) (map[string]string, error) {
	repos, err := st.Repositories(ctx)
	if err != nil {
		return nil, err
	}
	workers, err := st.Workers(ctx, now)
	if err != nil {
		return nil, err
	}
	connected := connectedWorkerIDs(workers)
	byRepo, err := targetsByRepository(ctx, st)
	if err != nil {
		return nil, err
	}
	out := make(map[string]string, len(repos))
	for _, r := range repos {
		out[r.Name] = repoState(r.Paused, r.WorkerID != "" && connected[r.WorkerID], byRepo[r.Name], now)
	}
	return out, nil
}

// connectedWorkerIDs is the set of workers counting as connected right now.
func connectedWorkerIDs(workers []store.Worker) map[string]bool {
	out := make(map[string]bool, len(workers))
	for _, w := range workers {
		if w.Connected {
			out[w.ID] = true
		}
	}
	return out
}

// targetsByRepository groups every recent Target under its repository, for the
// state derivation.
func targetsByRepository(ctx context.Context, st *store.Store) (map[string][]store.Target, error) {
	works, err := st.ListWork(ctx, 200)
	if err != nil {
		return nil, err
	}
	ids := make([]string, len(works))
	for i, w := range works {
		ids[i] = w.ID
	}
	byWork, err := st.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, err
	}
	out := map[string][]store.Target{}
	for _, ts := range byWork {
		for _, t := range ts {
			out[t.Repository] = append(out[t.Repository], t)
		}
	}
	return out, nil
}

// repoSummary is one row of GET /api/v1/repositories: the repository plus its
// derived state.
type repoSummary struct {
	store.Repository
	State string `json:"state"`
}

// repoDetail is GET /api/v1/repositories/{name}: the repository, its state, the
// tasks that touch it (running and recent), its retained worktrees, and the
// checks its forge.toml declares.
type repoDetail struct {
	Repository    store.Repository         `json:"repository"`
	State         string                   `json:"state"`
	Running       []workSummary            `json:"running"`
	Recent        []workSummary            `json:"recent"`
	RetainedCount int                      `json:"retained_count"`
	Retained      []store.RetainedWorktree `json:"retained"`
	Checks        []string                 `json:"checks"`
	Paused        bool                     `json:"paused"`
	AppURL        string                   `json:"app_url,omitempty"`
}

// buildRepoDetail assembles the repository detail; the API handler and the UI
// page both call it so the shape has one home. ErrNotFound for an unknown one.
func buildRepoDetail(ctx context.Context, st *store.Store, now time.Time, name string) (*repoDetail, error) {
	repo, err := st.Repository(ctx, name)
	if err != nil {
		return nil, err
	}
	works, err := st.ListWork(ctx, 200)
	if err != nil {
		return nil, err
	}
	ids := make([]string, len(works))
	for i, w := range works {
		ids[i] = w.ID
	}
	byWork, err := st.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, err
	}
	var repoTargets []store.Target
	running, recent := []workSummary{}, []workSummary{}
	for _, w := range works {
		var mine []store.Target
		for _, t := range byWork[w.ID] {
			if t.Repository == name {
				mine = append(mine, t)
			}
		}
		if len(mine) == 0 {
			continue
		}
		repoTargets = append(repoTargets, mine...)
		row := workSummary{Work: w, State: model.DeriveWorkState(model.WorkInputs{Targets: targetStates(mine), Integrate: w.Integrate}), Targets: mine}
		if anyActiveTarget(mine) {
			running = append(running, row)
		} else {
			recent = append(recent, row)
		}
	}
	workers, err := st.Workers(ctx, now)
	if err != nil {
		return nil, err
	}
	connected := repo.WorkerID != "" && connectedWorkerIDs(workers)[repo.WorkerID]
	retained, err := st.RetainedWorktreesForRepository(ctx, name)
	if err != nil {
		return nil, err
	}
	if retained == nil {
		retained = []store.RetainedWorktree{}
	}
	checks := checksFromForgeToml(repo.ForgeToml)
	return &repoDetail{
		Repository: *repo, State: repoState(repo.Paused, connected, repoTargets, now),
		Running: running, Recent: recent, RetainedCount: len(retained), Retained: retained,
		Checks: checks, Paused: repo.Paused, AppURL: repo.AppURL,
	}, nil
}

// anyActiveTarget reports whether any of a task's targets on the repository is
// active (so the task is running for this repository).
func anyActiveTarget(ts []store.Target) bool {
	for _, t := range ts {
		if activeTargetState(t.State) {
			return true
		}
	}
	return false
}

// checksFromForgeToml reads the declared check names out of the repository's
// stored forge.toml column (the worker's JSON-encoded ForgeToml, Go field
// names, no tags). An undecodable column yields no checks, never an error.
func checksFromForgeToml(col string) []string {
	if col == "" {
		return []string{}
	}
	var ft struct {
		Checks map[string][]string
	}
	if json.Unmarshal([]byte(col), &ft) != nil {
		return []string{}
	}
	names := make([]string, 0, len(ft.Checks))
	for n := range ft.Checks {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}

// repoName reads and validates the {name} path segment.
func repoName(r *http.Request) (string, error) {
	name := r.PathValue("name")
	if err := model.ValidateName(name); err != nil {
		return "", badRequest("%v", err)
	}
	return name, nil
}

func (s *Server) getRepository(r *http.Request) (int, any, error) {
	name, err := repoName(r)
	if err != nil {
		return 0, nil, err
	}
	detail, err := buildRepoDetail(r.Context(), s.store, s.now(), name)
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, detail, nil
}

func (s *Server) pauseRepository(r *http.Request) (int, any, error) {
	return s.setRepositoryPaused(r, true)
}

func (s *Server) resumeRepository(r *http.Request) (int, any, error) {
	return s.setRepositoryPaused(r, false)
}

// setRepositoryPaused flips the pause flag and answers with the updated
// repository; drain-guarded like the other operator writes.
func (s *Server) setRepositoryPaused(r *http.Request, paused bool) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	name, err := repoName(r)
	if err != nil {
		return 0, nil, err
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.SetRepositoryPaused(ctx, name, paused) }); err != nil {
		return 0, nil, err
	}
	repo, err := s.store.Repository(ctx, name)
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "repository pause set", "repository", name, "paused", paused)
	return http.StatusOK, repo, nil
}

// cancelRepositoryRunning cancels every task with an active target on the
// repository, reusing the ordinary cancel path (CancelWork). Like cancelWork it
// is exempt from the drain refusal: cancelling hastens a drain.
func (s *Server) cancelRepositoryRunning(r *http.Request) (int, any, error) {
	ctx := r.Context()
	name, err := repoName(r)
	if err != nil {
		return 0, nil, err
	}
	if _, err := s.store.Repository(ctx, name); err != nil {
		return 0, nil, err
	}
	works, err := s.store.ListWork(ctx, 200)
	if err != nil {
		return 0, nil, err
	}
	ids := make([]string, len(works))
	for i, w := range works {
		ids[i] = w.ID
	}
	byWork, err := s.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return 0, nil, err
	}
	var toCancel []string
	for _, w := range works {
		var mine []store.Target
		for _, t := range byWork[w.ID] {
			if t.Repository == name {
				mine = append(mine, t)
			}
		}
		if anyActiveTarget(mine) {
			toCancel = append(toCancel, w.ID)
		}
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		for _, id := range toCancel {
			if err := tx.CancelWork(ctx, id, "human"); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "repository running cancelled", "repository", name, "cancelled", len(toCancel))
	return http.StatusOK, map[string]int{"cancelled": len(toCancel)}, nil
}

// appURLRequest is POST /api/v1/repositories/{name}/app-url's body; an empty
// url clears the link.
type appURLRequest struct {
	URL string `json:"url"`
}

func (s *Server) setRepositoryAppURL(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	name, err := repoName(r)
	if err != nil {
		return 0, nil, err
	}
	var req appURLRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	link := strings.TrimSpace(req.URL)
	if err := validateAppURL(link); err != nil {
		return 0, nil, badRequest("%v", err)
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.SetRepositoryAppURL(ctx, name, link) }); err != nil {
		return 0, nil, err
	}
	repo, err := s.store.Repository(ctx, name)
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "repository app url set", "repository", name, "cleared", link == "")
	return http.StatusOK, repo, nil
}

// validateAppURL accepts the empty string (clear) or an absolute http(s) URL.
func validateAppURL(raw string) error {
	if raw == "" {
		return nil
	}
	u, err := url.Parse(raw)
	if err != nil {
		return fmt.Errorf("app url: %v", err)
	}
	if u.Scheme != "http" && u.Scheme != "https" {
		return fmt.Errorf("app url must be an http or https URL")
	}
	if u.Host == "" {
		return fmt.Errorf("app url must include a host")
	}
	return nil
}

// addRepoRequest is POST /api/v1/repositories: give a local path (or bare name
// under projects_root) or a remote URL to clone. name overrides the clone's
// directory name.
type addRepoRequest struct {
	Path string `json:"path"`
	URL  string `json:"url"`
	Name string `json:"name"`
}

// addRepository clones (if a URL) and registers a repository; the worker picks
// it up on its next refresh, so the created row appears a beat later. The daemon
// hook does the filesystem work; a nil hook (e.g. under test) disables the route.
func (s *Server) addRepository(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	if s.addRepo == nil {
		return 0, nil, badRequest("adding repositories is not available in this process")
	}
	var req addRepoRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	req.Path, req.URL, req.Name = strings.TrimSpace(req.Path), strings.TrimSpace(req.URL), strings.TrimSpace(req.Name)
	if req.Path == "" && req.URL == "" {
		return 0, nil, badRequest("a local path or a remote url is required")
	}
	repo, err := s.addRepo(ctx, req.Path, req.URL, req.Name)
	if err != nil {
		return 0, nil, badRequest("%v", err)
	}
	s.log.InfoContext(ctx, "repository added", "repository", repo.Name, "path", repo.Path, "cloned", req.URL != "")
	return http.StatusCreated, repo, nil
}

// archiveRepository frees disk without losing history: it deletes the checkout
// and drops the repository from worker.toml while keeping the row and every
// fact/attempt/kb note about it. It refuses while work is running — deleting the
// checkout would break that attempt's linked worktrees.
func (s *Server) archiveRepository(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	if s.archiveRepo == nil {
		return 0, nil, badRequest("archiving repositories is not available in this process")
	}
	name, err := repoName(r)
	if err != nil {
		return 0, nil, err
	}
	repo, err := s.store.Repository(ctx, name)
	if err != nil {
		return 0, nil, err
	}
	if repo.Archived {
		return 0, nil, badRequest("repository %s is already archived", name)
	}
	active, err := s.repositoryHasActiveWork(ctx, name)
	if err != nil {
		return 0, nil, err
	}
	if active {
		return 0, nil, badRequest("repository %s has running work; cancel it first", name)
	}
	originURL, err := s.archiveRepo(ctx, name, repo.Path)
	if err != nil {
		return 0, nil, badRequest("%v", err)
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.SetRepositoryArchived(ctx, name, true, originURL) }); err != nil {
		return 0, nil, err
	}
	updated, err := s.store.Repository(ctx, name)
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "repository archived", "repository", name, "has_origin", originURL != "")
	return http.StatusOK, updated, nil
}

// restoreRepository re-clones an archived repository from its saved origin URL
// and re-registers it.
func (s *Server) restoreRepository(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	if s.restoreRepo == nil {
		return 0, nil, badRequest("restoring repositories is not available in this process")
	}
	name, err := repoName(r)
	if err != nil {
		return 0, nil, err
	}
	repo, err := s.store.Repository(ctx, name)
	if err != nil {
		return 0, nil, err
	}
	if !repo.Archived {
		return 0, nil, badRequest("repository %s is not archived", name)
	}
	if _, err := s.restoreRepo(ctx, name, repo.OriginURL); err != nil {
		return 0, nil, badRequest("%v", err)
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.SetRepositoryArchived(ctx, name, false, "") }); err != nil {
		return 0, nil, err
	}
	updated, err := s.store.Repository(ctx, name)
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "repository restored", "repository", name)
	return http.StatusOK, updated, nil
}

// repositoryHasActiveWork reports whether any target of the repository is in an
// active (claimed/running/…) state — the guard archival needs.
func (s *Server) repositoryHasActiveWork(ctx context.Context, name string) (bool, error) {
	works, err := s.store.ListWork(ctx, 200)
	if err != nil {
		return false, err
	}
	ids := make([]string, len(works))
	for i, w := range works {
		ids[i] = w.ID
	}
	byWork, err := s.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return false, err
	}
	for _, ts := range byWork {
		var mine []store.Target
		for _, t := range ts {
			if t.Repository == name {
				mine = append(mine, t)
			}
		}
		if anyActiveTarget(mine) {
			return true, nil
		}
	}
	return false, nil
}

// appActionRepo resolves the repository (for its checkout path) shared by the
// app lifecycle handlers; archived or unknown repositories are refused.
func (s *Server) appActionRepo(r *http.Request) (*store.Repository, error) {
	name, err := repoName(r)
	if err != nil {
		return nil, err
	}
	repo, err := s.store.Repository(r.Context(), name)
	if err != nil {
		return nil, err
	}
	if repo.Archived {
		return nil, badRequest("repository %s is archived", name)
	}
	return repo, nil
}

// appStatusHandler is GET /api/v1/repositories/{name}/app: the run state the
// Repos page polls. A nil supervisor reports a stopped, unconfigured app.
func (s *Server) appStatusHandler(r *http.Request) (int, any, error) {
	repo, err := s.appActionRepo(r)
	if err != nil {
		return 0, nil, err
	}
	if s.appStatus == nil {
		return http.StatusOK, protocol.AppStatus{State: "stopped"}, nil
	}
	st, err := s.appStatus(r.Context(), repo.Name, repo.Path)
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, st, nil
}

func (s *Server) appStart(r *http.Request) (int, any, error)   { return s.appAction(r, s.startApp) }
func (s *Server) appStop(r *http.Request) (int, any, error)    { return s.appAction(r, s.stopApp) }
func (s *Server) appRebuild(r *http.Request) (int, any, error) { return s.appAction(r, s.rebuildApp) }

// appAction runs one supervised app command and answers with the new status.
func (s *Server) appAction(r *http.Request, fn func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)) (int, any, error) {
	if s.Draining() {
		return 0, nil, errDraining
	}
	if fn == nil {
		return 0, nil, badRequest("the app lifecycle is not available in this process")
	}
	repo, err := s.appActionRepo(r)
	if err != nil {
		return 0, nil, err
	}
	st, err := fn(r.Context(), repo.Name, repo.Path)
	if err != nil {
		return 0, nil, badRequest("%v", err)
	}
	return http.StatusOK, st, nil
}
