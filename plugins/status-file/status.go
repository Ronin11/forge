// status.go is the file's contract: the exact status.json shape of
// docs/PLUGINS.md, the one state-precedence function, and gather, which
// assembles a snapshot from the daemon's API. Everything here is pure with
// respect to the API interface so tests run against a fake, never a daemon.
package main

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"time"
)

// forgeAPI is every read gather performs, so tests substitute a fake. The
// concrete implementation is *client.
type forgeAPI interface {
	Queue(ctx context.Context) ([]queueItem, error)
	Task(ctx context.Context, id string) (*taskDetail, error)
	Attention(ctx context.Context) (*attentionView, error)
	// Usage returns (nil, nil) when the daemon has no active budget policy.
	Usage(ctx context.Context) (*usageView, error)
}

// status is status.json, schema 1 (docs/PLUGINS.md). Field order is the
// published order; optional fields that the daemon cannot cheaply answer
// (phase, tokens) are omitted rather than guessed.
type status struct {
	Schema      int           `json:"schema"`
	TS          string        `json:"ts"`
	Daemon      string        `json:"daemon"`
	State       string        `json:"state"`
	Running     []runningItem `json:"running"`
	Queued      int           `json:"queued"`
	Blocked     int           `json:"blocked"`
	Deferred    int           `json:"deferred"`
	HumanQueue  humanQueue    `json:"human_queue"`
	Usage       *usageBlock   `json:"usage,omitempty"`
	LastFailure *lastFailure  `json:"last_failure,omitempty"`
	UI          string        `json:"ui"`
}

type runningItem struct {
	Work     string `json:"work"`
	Title    string `json:"title"`
	Routine  string `json:"routine,omitempty"`
	Repo     string `json:"repo"`
	Mode     string `json:"mode,omitempty"`
	Phase    string `json:"phase,omitempty"`
	ElapsedS int64  `json:"elapsed_s,omitempty"`
	Tokens   int64  `json:"tokens,omitempty"`
	Model    string `json:"model,omitempty"`
	URL      string `json:"url"`
}

type humanQueue struct {
	Questions     int         `json:"questions"`
	Proposals     int         `json:"proposals"`
	Verifications int         `json:"verifications"`
	Items         []humanItem `json:"items"`
}

type humanItem struct {
	Kind  string `json:"kind"`
	ID    string `json:"id"`
	Title string `json:"title"`
	AgeS  int64  `json:"age_s"`
	URL   string `json:"url"`
}

type usageBlock struct {
	FiveHour usageWindow `json:"five_hour"`
	SevenDay usageWindow `json:"seven_day"`
}

type usageWindow struct {
	Utilization float64 `json:"utilization"`
	ResetsInS   int64   `json:"resets_in_s"`
	Target      float64 `json:"target"`
}

type lastFailure struct {
	Work    string `json:"work"`
	Routine string `json:"routine,omitempty"`
	Reason  string `json:"reason"`
	TS      string `json:"ts"`
	URL     string `json:"url"`
}

// computeState is the one home of the state rule (docs/PLUGINS.md): throttled
// beats attention beats working beats idle. Nothing else decides the state.
func computeState(throttled bool, humanQueue int, failedRecently, running bool) string {
	switch {
	case throttled:
		return "throttled"
	case humanQueue > 0 || failedRecently:
		return "attention"
	case running:
		return "working"
	default:
		return "idle"
	}
}

// maxHumanItems bounds the file: a bar shows a handful, and the UI has the rest.
const maxHumanItems = 20

// failureWindow is how long a failure keeps the state at attention.
const failureWindow = time.Hour

// gather reads the daemon's view and assembles one status snapshot. failures
// is the recent slice from the ring (oldest first). The queue read is the one
// hard dependency; attention rides on the same daemon, so its error also
// fails the snapshot, while a usage error only omits the usage block (the
// budget policy may be inactive, and hard stops still surface through the
// queue's deferred reasons).
func gather(ctx context.Context, api forgeAPI, now time.Time, failures []failureRecord, ui string) (*status, error) {
	q, err := api.Queue(ctx)
	if err != nil {
		return nil, fmt.Errorf("read queue: %w", err)
	}
	st := &status{Schema: 1, TS: now.UTC().Format(time.RFC3339), Daemon: "ok", Running: []runningItem{}, UI: ui}
	throttled := false
	runningWork := false
	for _, item := range q {
		switch item.State {
		case "pending":
			st.Queued++
		case "blocked":
			st.Blocked++
		case "deferred":
			st.Deferred++
			// The budget policy defers with reason "hard_stop:<window>" when a
			// hard stop is active — the throttle signal even without usage:read.
			if strings.HasPrefix(item.Reason, "hard_stop") {
				throttled = true
			}
		case "running":
			runningWork = true
			st.Running = append(st.Running, runningItems(ctx, api, item, now, ui)...)
		}
	}

	att, err := api.Attention(ctx)
	if err != nil {
		return nil, fmt.Errorf("read attention: %w", err)
	}
	st.HumanQueue = humanQueueOf(att, now, ui)

	if uv, err := api.Usage(ctx); err == nil && uv != nil {
		st.Usage = &usageBlock{
			FiveHour: windowOut(uv.Usage.FiveHour, uv.Config.FiveHourTarget, now),
			SevenDay: windowOut(uv.Usage.SevenDay, uv.Config.SevenDayTarget, now),
		}
		throttled = throttled || hardStopped(uv.Usage.FiveHour, uv.Config.FiveHourHardStop) ||
			hardStopped(uv.Usage.SevenDay, uv.Config.SevenDayHardStop)
	}

	if len(failures) > 0 {
		f := failures[len(failures)-1]
		st.LastFailure = &lastFailure{Work: f.Work, Routine: f.Routine, Reason: f.Reason,
			TS: f.At.UTC().Format(time.RFC3339), URL: ui + "/tasks/" + f.Work}
	}

	hq := st.HumanQueue.Questions + st.HumanQueue.Proposals + st.HumanQueue.Verifications
	st.State = computeState(throttled, hq, len(failures) > 0, runningWork || len(st.Running) > 0)
	return st, nil
}

// runningItems builds one entry per active target of a running Work. The queue
// row already carries repo, title, routine, and start time; mode and model come
// from the task detail, best effort — a failed detail read degrades the entry,
// never the snapshot.
func runningItems(ctx context.Context, api forgeAPI, item queueItem, now time.Time, ui string) []runningItem {
	detail, err := api.Task(ctx, item.Work.ID)
	if err != nil {
		detail = nil
	}
	var out []runningItem
	for _, t := range item.Targets {
		if !activeTarget(t.State) {
			continue
		}
		ri := runningItem{Work: item.Work.ID, Title: item.Work.Title, Routine: item.Work.RoutineName,
			Repo: t.Repository, URL: ui + "/tasks/" + item.Work.ID}
		started := t.StartedAt
		if detail != nil {
			if a := latestAttempt(detail.Attempts, t.ID); a != nil {
				ri.Mode, ri.Model = a.Mode, a.ModelAlias
				if started.IsZero() {
					started = a.StartedAt
				}
			}
		}
		if !started.IsZero() && now.After(started) {
			ri.ElapsedS = int64(now.Sub(started).Seconds())
		}
		out = append(out, ri)
	}
	return out
}

// activeTarget mirrors model.Active: the states that hold a worker slot.
func activeTarget(s string) bool { return s == "claimed" || s == "preparing" || s == "running" }

// latestAttempt picks the target's newest attempt (attempts arrive oldest
// first per target from the API).
func latestAttempt(attempts []attemptView, targetID string) *attemptView {
	var last *attemptView
	for i := range attempts {
		if attempts[i].TargetID == targetID {
			last = &attempts[i]
		}
	}
	return last
}

func humanQueueOf(att *attentionView, now time.Time, ui string) humanQueue {
	hq := humanQueue{Items: []humanItem{}}
	hq.Questions = len(att.Questions)
	hq.Proposals = len(att.Proposals)
	// Verifications are not served by /api/v1/attention yet; the count is an
	// honest zero until the endpoint grows them.
	for _, q := range att.Questions {
		if len(hq.Items) >= maxHumanItems {
			break
		}
		hq.Items = append(hq.Items, humanItem{Kind: "question", ID: q.ID, Title: firstLine(q.Text),
			AgeS: ageSeconds(now, q.AskedAt), URL: ui + "/tasks/" + q.WorkID})
	}
	for _, p := range att.Proposals {
		if len(hq.Items) >= maxHumanItems {
			break
		}
		hq.Items = append(hq.Items, humanItem{Kind: "proposal", ID: p.ID, Title: p.Kind + ": " + p.Target,
			AgeS: ageSeconds(now, p.CreatedAt), URL: ui + "/proposals"})
	}
	return hq
}

func windowOut(w windowView, target float64, now time.Time) usageWindow {
	out := usageWindow{Utilization: w.Utilization, Target: target}
	if !w.ResetsAt.IsZero() {
		if d := w.ResetsAt.Sub(now); d > 0 {
			out.ResetsInS = int64(d.Seconds())
		}
	}
	return out
}

// hardStopped is the throttle test for one window: a configured hard stop
// (0 = none) met by the sampled utilization (-1 = no sample yet).
func hardStopped(w windowView, hardStop float64) bool {
	return hardStop > 0 && w.Utilization >= hardStop
}

func ageSeconds(now, then time.Time) int64 {
	if then.IsZero() || now.Before(then) {
		return 0
	}
	return int64(now.Sub(then).Seconds())
}

// firstLine is a human item's title: the text's first line, bounded.
func firstLine(s string) string {
	if i := strings.IndexByte(s, '\n'); i >= 0 {
		s = s[:i]
	}
	r := []rune(s)
	if len(r) > 80 {
		return string(r[:79]) + "…"
	}
	return s
}

// relevant reports whether a journal kind should trigger a rewrite. The
// heartbeat ticker covers everything else.
func relevant(kind string) bool {
	for _, p := range []string{"work.", "target.", "question.", "proposal.", "daemon.", "budget."} {
		if strings.HasPrefix(kind, p) {
			return true
		}
	}
	return false
}

// failureRecord is one target failure seen in the journal.
type failureRecord struct {
	JournalID int64
	Work      string
	Routine   string
	Reason    string
	At        time.Time
}

// ringCap bounds the in-memory failure history; only the last hour matters.
const ringCap = 32

// failureRing keeps the recent failures the journal revealed. After a restart
// it is empty until events arrive (or the startup scan finds recent ones);
// docs/PLUGINS.md records that as acceptable.
type failureRing struct {
	mu      sync.Mutex // guards entries
	entries []failureRecord
}

// add appends one record, newest last, deduplicating by journal id (a
// reconnect may replay entries past the last ack).
func (r *failureRing) add(f failureRecord) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, e := range r.entries {
		if e.JournalID == f.JournalID {
			return
		}
	}
	r.entries = append(r.entries, f)
	if len(r.entries) > ringCap {
		r.entries = r.entries[len(r.entries)-ringCap:]
	}
}

// recent returns the records within window of now, oldest first.
func (r *failureRing) recent(now time.Time, window time.Duration) []failureRecord {
	r.mu.Lock()
	defer r.mu.Unlock()
	var out []failureRecord
	for _, e := range r.entries {
		if now.Sub(e.At) <= window {
			out = append(out, e)
		}
	}
	return out
}
