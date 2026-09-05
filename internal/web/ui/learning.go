package ui

import (
	"context"
	"encoding/json"
	"net/http"
	"os/exec"
	"sort"
	"strconv"
	"strings"
	"time"

	"forge/internal/core/engine"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// The Learning page: one time-ordered feed of everything the self-improvement
// loop did and what became of it — reflection runs (landed, refuted by the
// verifier, or failed), library commits with the proposals that made or
// reverted them, experiments with their decisions, and scratch promotions.
// Composed entirely from existing records: the works table, the proposals
// table, the experiments table, and the prompts library's git log.

type learningEntry struct {
	At         time.Time
	Kind       string // reflection | promotion | commit | experiment | proposal
	Status     string // landed | refuted | reverted | promoted | kept_control | live | ...
	Title      string
	Detail     string
	Directives []string
	Link       string // in-app drill-down, "" when none
	Ref        string // short external identity: commit sha, experiment id
	RefLink    string // where Ref leads (the commit diff view), "" when nowhere
	ProposalID string // annotating proposal, "" when none
	Cost       string // "$0.77", "-" when unknown
	Metrics    string // compact outcome numbers (A/B verdict, arm rates)
}

type learningData struct {
	Entries []learningEntry
	// Rollup header.
	Landed, Refuted, Reverted int
	ExpOpen, ExpPromoted      int
	SpendUSD                  float64
	// The [learning] pool: rolling-7d API-billed spend against the weekly
	// budget (subscription spend is prepaid and governed by capacity).
	WeekSpendUSD, BudgetUSD float64
	// Capacity is the subscription-window line ("" when no policy is wired).
	Capacity string
	// Calibration is the prediction ledger per source ("experiment 3/4 held").
	Calibration []store.CalibrationRow
}

func (u *UI) learning(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	var data learningData
	data.BudgetUSD = u.learningCfg.BudgetUSDPerWeek
	if spent, err := u.store.LearningAPISpendSince(ctx, u.clock().Add(-7*24*time.Hour), u.apiRunners); err == nil {
		data.WeekSpendUSD = spent
	}
	data.Capacity = u.capacityLine(ctx)
	if cal, err := u.store.Calibration(ctx, u.clock().Add(-30*24*time.Hour)); err == nil {
		data.Calibration = cal
	}

	proposals, err := u.store.ListProposals(ctx, "")
	if err != nil {
		u.fail(w, r, err)
		return
	}
	// Proposals that landed as (or reverted) a library commit annotate that
	// commit's entry instead of appearing twice; the rest stand alone.
	bySha := map[string]*store.Proposal{}
	for i := range proposals {
		p := proposals[i]
		if _, sha, ok := cutFragmentRef(p.AppliedRef); ok {
			bySha[sha] = &proposals[i]
		}
	}

	works, err := u.store.LearningWorks(ctx, 100)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	for _, lw := range works {
		e := learningEntry{At: lw.CreatedAt, Kind: "reflection", Title: lw.Title, Link: "/tasks/" + lw.ID, Cost: "-"}
		if lw.CostUSD != nil {
			e.Cost = "$" + strconv.FormatFloat(*lw.CostUSD, 'f', 2, 64)
		}
		if lw.Cause == string(model.CausePromotion) {
			e.Kind = "promotion"
		}
		e.Status = learningWorkStatus(lw.State, lw.UnverifiedReason)
		var sha string
		e.Detail, e.Directives, sha = resultSummary(lw.Result)
		if sha != "" {
			// Merged edits resolve in the library; refuted ones land on the
			// commit view's "unpushed branch" explanation — both informative.
			e.Ref, e.RefLink = sha, "/learning/commits/"+sha
		}
		if lw.CostUSD != nil {
			data.SpendUSD += *lw.CostUSD
		}
		switch e.Status {
		case "landed":
			data.Landed++
		case "refuted":
			data.Refuted++
		}
		data.Entries = append(data.Entries, e)
	}

	for _, c := range libraryCommits(ctx, u.libraryDir(), 60) {
		e := learningEntry{At: c.at, Kind: "commit", Status: "landed", Title: c.subject, Directives: c.directives, Ref: c.sha, Cost: "-",
			Link: "/learning/commits/" + c.sha, RefLink: "/learning/commits/" + c.sha}
		if p := bySha[c.sha]; p != nil {
			e.ProposalID = p.ID
			e.Detail = "via " + p.Source
			if p.Status == model.ProposalReverted {
				e.Status = "reverted"
				e.Metrics = abVerdict(p.OutcomeMetrics)
				data.Reverted++
			}
		}
		data.Entries = append(data.Entries, e)
	}

	experiments, err := u.store.RecentExperiments(ctx, 50)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	for _, pe := range experiments {
		e := learningEntry{At: pe.CreatedAt, Kind: "experiment", Status: pe.Status, Title: pe.Goal, Ref: pe.ID, Cost: "-"}
		if name, ok := strings.CutPrefix(pe.Subject, "directive:"); ok {
			e.Directives = []string{name}
		} else if name, ok := strings.CutPrefix(pe.Subject, "persona:"); ok {
			e.Directives = []string{name}
		}
		if pe.Kind == store.ExperimentKindLive {
			e.Detail = "live · " + pe.Subject
			e.Metrics = liveVerdict(pe.Results)
			if _, sha, ok := cutFragmentRef(appliedRefOf(pe.Results)); ok {
				e.Ref, e.RefLink = sha, "/learning/commits/"+sha
			}
		} else {
			e.Detail = "offline · " + pe.Subject
		}
		switch pe.Status {
		case store.ExperimentLive, store.ExperimentRunning:
			data.ExpOpen++
		case store.ExperimentPromoted:
			data.ExpPromoted++
		}
		data.Entries = append(data.Entries, e)
	}

	for i := range proposals {
		p := proposals[i]
		if _, _, ok := cutFragmentRef(p.AppliedRef); ok {
			continue // shown on its commit entry
		}
		e := learningEntry{At: p.CreatedAt, Kind: "proposal", Status: string(p.Status), Title: p.Target, Link: "/proposals/" + p.ID, Detail: p.Rationale, Cost: "-"}
		if p.Status == model.ProposalReverted {
			e.Metrics = abVerdict(p.OutcomeMetrics)
			data.Reverted++
		}
		data.Entries = append(data.Entries, e)
	}

	if gens, err := u.store.RecentWorkflowGenerations(ctx, 30); err == nil {
		for _, g := range gens {
			e := learningEntry{At: g.CreatedAt, Kind: "workflow", Status: "landed", Cost: "-",
				Title: g.WorkflowName + " @ generation " + strconv.Itoa(g.Generation),
				Link:  "/workflows/" + g.WorkflowName + "/edit", Detail: "via " + g.Source}
			if pid, ok := strings.CutPrefix(g.Source, "proposal:"); ok {
				e.ProposalID = pid
			}
			if strings.HasPrefix(g.Source, "rollback:") {
				e.Status = "reverted"
			}
			data.Entries = append(data.Entries, e)
		}
	}

	sort.SliceStable(data.Entries, func(i, j int) bool { return data.Entries[i].At.After(data.Entries[j].At) })
	u.render(w, r, "learning.html", "Learning", data)
}

// libraryDir is the prompts library's git checkout; "" when the UI runs
// without a daemon (tests) or the library is not wired.
func (u *UI) libraryDir() string {
	if u.prompts == nil {
		return ""
	}
	if lib := u.prompts(); lib != nil {
		return lib.Dir
	}
	return ""
}

// learningWorkStatus folds a target's fate into the feed vocabulary. The
// distinction that matters: merged means the library edit landed; unverified
// with a verify_verdict reason means the verifier read the claims and
// refuted them — the loop's skepticism doing its job.
func learningWorkStatus(state, unverifiedReason string) string {
	switch state {
	case "merged", "succeeded":
		return "landed"
	case "unverified":
		if strings.HasPrefix(unverifiedReason, "verify_verdict") {
			return "refuted"
		}
		return "unverified"
	case "failed", "cancelled":
		return state
	default:
		return "running"
	}
}

// resultSummary lifts the envelope's summary line, the directive names its
// changes touched (directives/plan-project.md → plan-project), and the first
// commit sha it recorded.
func resultSummary(raw json.RawMessage) (string, []string, string) {
	if len(raw) == 0 {
		return "", nil, ""
	}
	var env struct {
		Summary string `json:"summary"`
		Changes []struct {
			Path string `json:"path"`
		} `json:"changes"`
		Commits []struct {
			SHA string `json:"sha"`
		} `json:"commits"`
	}
	if err := json.Unmarshal(raw, &env); err != nil {
		return "", nil, ""
	}
	var names []string
	for _, c := range env.Changes {
		if n := fragmentName(c.Path); n != "" {
			names = append(names, n)
		}
	}
	sha := ""
	if len(env.Commits) > 0 && shaPattern.MatchString(env.Commits[0].SHA) {
		sha = env.Commits[0].SHA
	}
	return env.Summary, names, sha
}

// appliedRefOf lifts applied_ref from a live experiment's results blob.
func appliedRefOf(raw json.RawMessage) string {
	var res struct {
		AppliedRef string `json:"applied_ref"`
	}
	if json.Unmarshal(raw, &res) != nil {
		return ""
	}
	return res.AppliedRef
}

// cutFragmentRef splits "directive:<name>@<sha>" / "persona:<name>@<sha>"
// applied refs; anything else (generation:N, script refs) reports false.
func cutFragmentRef(ref string) (name, sha string, ok bool) {
	rest, found := strings.CutPrefix(ref, "directive:")
	if !found {
		rest, found = strings.CutPrefix(ref, "persona:")
	}
	if !found {
		return "", "", false
	}
	name, sha, found = strings.Cut(rest, "@")
	if !found || name == "" || sha == "" {
		return "", "", false
	}
	return name, sha, true
}

// fragmentName strips the library layout from a changed path: any of the
// fragment directories, then the .md suffix.
func fragmentName(path string) string {
	for _, dir := range []string{"directives/", "personas/", "fragments/", "scripts/"} {
		if rest, ok := strings.CutPrefix(path, dir); ok {
			return strings.TrimSuffix(rest, ".md")
		}
	}
	return ""
}

// abVerdict compacts a reverted proposal's A/B outcome metrics into one line.
func abVerdict(raw json.RawMessage) string {
	if len(raw) == 0 {
		return ""
	}
	var m struct {
		RegressedOn string  `json:"regressed_on"`
		PrevRate    float64 `json:"prev_rate"`
		NewRate     float64 `json:"new_rate"`
	}
	if err := json.Unmarshal(raw, &m); err != nil || m.RegressedOn == "" {
		return ""
	}
	return "regressed on " + m.RegressedOn + ": " + pct(m.PrevRate) + " → " + pct(m.NewRate)
}

// liveVerdict compacts a live experiment's per-arm results into one line.
func liveVerdict(raw json.RawMessage) string {
	if len(raw) == 0 {
		return ""
	}
	var res struct {
		Winner string `json:"winner"`
		Reason string `json:"reason"`
		Arms   []struct {
			Label        string  `json:"label"`
			Runs         int     `json:"runs"`
			VerifiedRate float64 `json:"verified_rate"`
		} `json:"arms"`
	}
	if err := json.Unmarshal(raw, &res); err != nil {
		return ""
	}
	var parts []string
	for _, a := range res.Arms {
		parts = append(parts, a.Label+" "+pct(a.VerifiedRate)+" ("+strconv.Itoa(a.Runs)+")")
	}
	line := strings.Join(parts, " · ")
	if res.Winner != "" {
		line = "winner " + res.Winner + " — " + line
	} else if res.Reason != "" && line == "" {
		line = res.Reason
	}
	return line
}

func pct(v float64) string { return strconv.Itoa(int(v*100+0.5)) + "%" }

// libraryCommit is one parsed git-log row from the prompts library.
type libraryCommit struct {
	sha        string
	at         time.Time
	subject    string
	directives []string
}

// libraryCommits reads the newest commits of the prompts library checkout.
// A missing or non-git dir yields nothing — the feed just has no commit rows.
func libraryCommits(ctx context.Context, dir string, limit int) []libraryCommit {
	if dir == "" {
		return nil
	}
	cctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	out, err := exec.CommandContext(cctx, "git", "-C", dir, "log", "-n", strconv.Itoa(limit),
		"--pretty=format:%x1e%H%x1f%cI%x1f%s", "--name-only").Output()
	if err != nil {
		return nil
	}
	var commits []libraryCommit
	for _, block := range strings.Split(string(out), "\x1e") {
		block = strings.TrimSpace(block)
		if block == "" {
			continue
		}
		lines := strings.Split(block, "\n")
		fields := strings.Split(lines[0], "\x1f")
		if len(fields) != 3 {
			continue
		}
		c := libraryCommit{sha: fields[0], subject: fields[2]}
		if t, err := time.Parse(time.RFC3339, fields[1]); err == nil {
			c.at = t
		}
		seen := map[string]bool{}
		for _, f := range lines[1:] {
			if n := fragmentName(strings.TrimSpace(f)); n != "" && !seen[n] {
				seen[n] = true
				c.directives = append(c.directives, n)
			}
		}
		commits = append(commits, c)
	}
	return commits
}

// capacityLine renders the subscription windows for the Learning header:
// utilization vs the burn-down pace, and what the last reset left unspent —
// the use-it-or-lose-it number the loop exists to spend.
func (u *UI) capacityLine(ctx context.Context) string {
	if u.usage == nil {
		return ""
	}
	usage, err := u.usage(ctx)
	if err != nil {
		return ""
	}
	part := func(w engine.WindowUsage, label string) string {
		if w.Utilization < 0 {
			return ""
		}
		line := label + " " + strconv.Itoa(int(w.Utilization*100+0.5)) + "% used"
		if w.FractionElapsed > 0 {
			line += " (pace " + strconv.Itoa(int(w.FractionElapsed*100+0.5)) + "%)"
		}
		if w.LastResetUnspent != nil && *w.LastResetUnspent > 0 {
			line += ", last reset wasted " + strconv.Itoa(int(*w.LastResetUnspent*100+0.5)) + "%"
		}
		return line
	}
	five, seven := part(usage.FiveHour, "5h"), part(usage.SevenDay, "7d")
	switch {
	case five == "" && seven == "":
		return ""
	case five == "":
		return seven
	case seven == "":
		return five
	}
	return five + " · " + seven
}
