package controlplane

// The Human queue's dynamic actions: each card carries the click that starts
// what the human actually has to do — open the doc a proposal covers, or fire
// a named daemon trigger (a test toast) the asking agent requested. Links and
// triggers come from agent-written question context, so both are constrained:
// links to same-origin paths, posts to the registry below — a mislabeled
// button must never reach an endpoint that decides anything.

import (
	"encoding/json"
	"strings"

	"forge/internal/model"
	"forge/internal/store"
)

// QueueAction is one click on a Human-queue card: a link (URL set) or a
// button posting to a registered trigger endpoint (Post set).
type QueueAction struct {
	Label string
	URL   string
	Post  string
}

// queueTriggers maps the trigger names a question's context may carry to the
// endpoint the button posts to. A registry, not a passthrough: only listed
// endpoints are reachable from a card, and none of them decide anything.
var queueTriggers = map[string]string{
	"notify_test": "/api/v1/notify/test",
}

// maxQueueActions keeps a card sane when a context carries a long list.
const maxQueueActions = 4

// questionActions extracts the renderable actions from a question's
// agent-written context: {"actions":[{"label", "url"|"trigger"}]}. A url must
// be a same-origin absolute path; a trigger must be registered. Anything
// malformed is dropped, never an error — the card still renders.
func questionActions(raw json.RawMessage) []QueueAction {
	if len(raw) == 0 {
		return nil
	}
	var qctx struct {
		Actions []struct {
			Label   string `json:"label"`
			URL     string `json:"url"`
			Trigger string `json:"trigger"`
		} `json:"actions"`
	}
	if json.Unmarshal(raw, &qctx) != nil {
		return nil
	}
	var out []QueueAction
	for _, a := range qctx.Actions {
		if len(out) == maxQueueActions {
			break
		}
		label := strings.TrimSpace(a.Label)
		if label == "" {
			continue
		}
		switch {
		case a.URL != "" && strings.HasPrefix(a.URL, "/") && !strings.HasPrefix(a.URL, "//"):
			out = append(out, QueueAction{Label: label, URL: a.URL})
		case a.URL == "" && a.Trigger != "":
			if post, ok := queueTriggers[a.Trigger]; ok {
				out = append(out, QueueAction{Label: label, Post: post})
			}
		}
	}
	return out
}

// proposalOpenURL is the click that opens the thing a proposal changes; ""
// when no page shows it. Today only a kb-note doc target has a page — a repo
// doc or mode prompt lives on disk, not in the UI.
func proposalOpenURL(p store.Proposal) string {
	if p.Kind == model.ProposalDoc {
		if id, ok := strings.CutPrefix(p.Target, "kb:"); ok && id != "" {
			return "/kb/" + id
		}
	}
	return ""
}
