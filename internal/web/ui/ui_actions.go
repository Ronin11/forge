package ui

// The Human queue's dynamic actions: each card carries the click that starts
// what the human actually has to do — a link (open the doc a proposal covers,
// jump to a page) or an action (fire a named, side-effect-safe RPC method like
// the test toast the asking agent requested). Both come from agent-written
// question context, so both are constrained: links to same-origin paths,
// actions to registered RPC methods (see rpcMethods) — a mislabeled button
// must never reach a URL pointed anywhere or a method that decides anything.

import (
	"encoding/json"
	"strings"

	"forge/internal/core/model"
	"forge/internal/core/store"
	"forge/internal/web"
)

// QueueAction is one click on a Human-queue card: a link (URL set) or an
// action (RPC set — a registered method name, POSTed to /api/v1/rpc/{method}
// with the optional Args JSON body).
type QueueAction struct {
	Label string
	URL   string
	RPC   string
	Args  string
}

// maxQueueActions keeps a card sane when a context carries a long list.
const maxQueueActions = 4

// questionActions extracts the renderable actions from a question's
// agent-written context: {"actions":[{"label", "url"|"rpc", "args"?}]}. A url
// must be a same-origin absolute path; an rpc must name a registered method
// (its args, if any, ride along as JSON). Anything malformed is dropped, never
// an error — the card still renders.
func questionActions(raw json.RawMessage) []QueueAction {
	if len(raw) == 0 {
		return nil
	}
	var qctx struct {
		Actions []struct {
			Label string          `json:"label"`
			URL   string          `json:"url"`
			RPC   string          `json:"rpc"`
			Args  json.RawMessage `json:"args"`
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
		case a.URL == "" && a.RPC != "":
			if web.RPCKnown(a.RPC) {
				out = append(out, QueueAction{Label: label, RPC: a.RPC, Args: string(a.Args)})
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
