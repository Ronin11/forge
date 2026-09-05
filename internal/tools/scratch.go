package tools

// forge_scratch: the organic script layer. An agent that needs a quick
// script saves-and-runs it here instead of a worktree throwaway — the daemon
// caches it (LRU-bounded), counts every run, makes it discoverable through
// forge_library, and when one keeps getting reached for it queues a
// high-priority curation Work that folds it into the git library (deduped,
// extended, or added — see the promote-scratch directive). The efficiency
// comes from reuse: search before writing, call by name after the first save.

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"forge/internal/core/model"
)

// ScratchInput is one forge_scratch call, validated by the tool before the
// daemon closure stores/executes it.
type ScratchInput struct {
	Name        string
	Language    string
	Source      string
	Description string
	InputSchema string
	Input       any
}

// ScratchMeta rides back with the output: the reuse signal, and the curation
// Work id when this run crossed the promotion threshold.
type ScratchMeta struct {
	RunCount      int
	PromotionWork string
}

// scratchLanguages bounds what an agent may submit; execution rules match
// the library (js = goja sandbox, others = daemon subprocess).
var scratchLanguages = map[string]bool{"js": true, "py": true, "sh": true, "rb": true, "pl": true}

type scratchTool struct{}

func (scratchTool) Name() string { return "forge_scratch" }
func (scratchTool) Description() string {
	return "Save and run a quick script, or re-run a cached one by name. SEARCH FIRST (forge_library kind=scratch) — reusing an existing scratch script by name is cheaper and more consistent than writing a new one, and scripts that keep being reused are promoted into the permanent library by a curation task. js runs sandboxed; py/sh/rb/pl run as daemon-side processes reading JSON input on stdin and printing one JSON value."
}
func (scratchTool) Where() string { return WhereDaemon }
func (scratchTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"name":{"type":"string","description":"lower-case slug; call with name only to re-run a cached script"},
		"source":{"type":"string","description":"the script (js: function main(input); others: JSON on stdin, one JSON value on stdout). Omit to run the cached version"},
		"language":{"type":"string","enum":["js","py","sh","rb","pl"],"description":"required with source"},
		"description":{"type":"string","description":"one line, required with source — it is how future searches find this"},
		"input_schema":{"type":"object","description":"JSON schema for input; include it so a promoted script becomes agent-callable"},
		"input":{"type":"object","description":"parameters; js sees input.params, subprocesses read {params: ...} on stdin"}
	},"required":["name"],"additionalProperties":false}`)
}

func (scratchTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	if req.Deps.Scratch == nil {
		return nil, fmt.Errorf("the scratch layer is unavailable in this process")
	}
	var in struct {
		Name        string          `json:"name"`
		Source      string          `json:"source"`
		Language    string          `json:"language"`
		Description string          `json:"description"`
		InputSchema json.RawMessage `json:"input_schema"`
		Input       any             `json:"input"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if err := model.ValidateName(in.Name); err != nil {
		return nil, BadInput("name: %v", err)
	}
	if in.Source != "" {
		if !scratchLanguages[in.Language] {
			return nil, BadInput("language %q: want js, py, sh, rb, or pl", in.Language)
		}
		if strings.TrimSpace(in.Description) == "" {
			return nil, BadInput("description is required with source — it is how this script is found again")
		}
		if strings.HasPrefix(in.Source, "/**forge") || strings.Contains(in.Source, "\n#forge\n") || strings.HasPrefix(in.Source, "#forge\n") {
			return nil, BadInput("do not embed a forge metadata header in scratch source — pass description/input_schema as fields; the curation task writes the header at promotion")
		}
		if in.Language == "js" && !strings.Contains(in.Source, "function main") {
			return nil, BadInput("a js scratch script must define function main(input)")
		}
	}
	schema := ""
	if len(in.InputSchema) > 0 {
		schema = string(in.InputSchema)
	}
	out, meta, err := req.Deps.Scratch(ctx, req.Attempt, ScratchInput{
		Name: in.Name, Language: in.Language, Source: in.Source,
		Description: strings.TrimSpace(in.Description), InputSchema: schema, Input: in.Input,
	})
	if err != nil {
		return nil, err
	}
	resp := map[string]any{"schema_version": SchemaVersion, "output": out, "run_count": meta.RunCount}
	if meta.PromotionWork != "" {
		resp["promotion_queued"] = meta.PromotionWork
		resp["note"] = "this script crossed the promotion threshold — a curation task is folding it into the permanent library; keep calling it here until it appears there"
	}
	return respond(resp)
}
