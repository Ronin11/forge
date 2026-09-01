package ui

import (
	"encoding/json"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

func TestQuestionActions(t *testing.T) {
	for name, tc := range map[string]struct {
		context string
		want    []QueueAction
	}{
		"empty":     {"", nil},
		"not json":  {"{", nil},
		"no acts":   {`{"note":"x"}`, nil},
		"link":      {`{"actions":[{"label":"Open doc","url":"/kb/setup"}]}`, []QueueAction{{Label: "Open doc", URL: "/kb/setup"}}},
		"rpc":       {`{"actions":[{"label":"Send test toast","rpc":"notify.test"}]}`, []QueueAction{{Label: "Send test toast", RPC: "notify.test"}}},
		"rpc args":  {`{"actions":[{"label":"Toast tasks","rpc":"notify.test","args":{"path":"/tasks"}}]}`, []QueueAction{{Label: "Toast tasks", RPC: "notify.test", Args: `{"path":"/tasks"}`}}},
		"absolute":  {`{"actions":[{"label":"evil","url":"https://evil.example"}]}`, nil},
		"schemeles": {`{"actions":[{"label":"evil","url":"//evil.example"}]}`, nil},
		"unknown":   {`{"actions":[{"label":"x","rpc":"approve.everything"}]}`, nil},
		"no label":  {`{"actions":[{"url":"/kb/setup"}]}`, nil},
		"both set":  {`{"actions":[{"label":"x","url":"/kb/a","rpc":"notify.test"}]}`, []QueueAction{{Label: "x", URL: "/kb/a"}}},
		"mixed": {`{"actions":[{"label":"a","url":"/tasks/1"},{"label":"bad","url":"http://x"},{"label":"b","rpc":"notify.test"}]}`,
			[]QueueAction{{Label: "a", URL: "/tasks/1"}, {Label: "b", RPC: "notify.test"}}},
	} {
		got := questionActions(json.RawMessage(tc.context))
		if len(got) != len(tc.want) {
			t.Errorf("%s: got %v, want %v", name, got, tc.want)
			continue
		}
		for i := range got {
			if got[i] != tc.want[i] {
				t.Errorf("%s[%d]: got %+v, want %+v", name, i, got[i], tc.want[i])
			}
		}
	}

	// The cap keeps a card sane.
	long := `{"actions":[{"label":"1","url":"/a"},{"label":"2","url":"/a"},{"label":"3","url":"/a"},{"label":"4","url":"/a"},{"label":"5","url":"/a"}]}`
	if got := questionActions(json.RawMessage(long)); len(got) != maxQueueActions {
		t.Errorf("cap: got %d actions, want %d", len(got), maxQueueActions)
	}
}

func TestProposalOpenURL(t *testing.T) {
	for name, tc := range map[string]struct {
		p    store.Proposal
		want string
	}{
		"kb doc":     {store.Proposal{Kind: model.ProposalDoc, Target: "kb:setup-guide"}, "/kb/setup-guide"},
		"repo doc":   {store.Proposal{Kind: model.ProposalDoc, Target: "repo:equitizr/forge.toml"}, ""},
		"empty note": {store.Proposal{Kind: model.ProposalDoc, Target: "kb:"}, ""},
		"routine":    {store.Proposal{Kind: model.ProposalRoutine, Target: "routine:inventory"}, ""},
	} {
		if got := proposalOpenURL(tc.p); got != tc.want {
			t.Errorf("%s: got %q, want %q", name, got, tc.want)
		}
	}
}
