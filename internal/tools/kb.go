package tools

// The knowledge tools: search, read, create, and traverse kb notes. Files are
// the truth (internal/kb); the store's index serves search and link queries.

import (
	"context"
	"encoding/json"
	"os"

	"forge/internal/core/kb"
	"forge/internal/store"
)

type kbSearchTool struct{}

func (kbSearchTool) Name() string { return "forge_kb_search" }
func (kbSearchTool) Description() string {
	return "Full-text search over kb note titles and bodies, best match first."
}
func (kbSearchTool) Where() string { return WhereDaemon }
func (kbSearchTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{"query":{"type":"string"},` + pageProps + `},"required":["query"],"additionalProperties":false}`)
}

func (kbSearchTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		Query string `json:"query"`
		page
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if err := in.clamp(); err != nil {
		return nil, err
	}
	if in.Query == "" {
		return nil, BadInput("query is required")
	}
	// SearchKb caps FTS results at 100 rows; fetch enough for the offset.
	fetch := min(in.Limit+in.Offset, 100)
	notes, err := req.Deps.Store.SearchKb(ctx, in.Query, fetch)
	if err != nil {
		return nil, err
	}
	notes = slicePage(notes, in.page)
	return respond(map[string]any{"schema_version": SchemaVersion, "notes": notes, "count": len(notes)})
}

type kbNoteTool struct{}

func (kbNoteTool) Name() string { return "forge_kb_note" }
func (kbNoteTool) Description() string {
	return "One kb note by id: the indexed metadata and the full Markdown body."
}
func (kbNoteTool) Where() string { return WhereDaemon }
func (kbNoteTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}`)
}

func (kbNoteTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		ID string `json:"id"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if in.ID == "" {
		return nil, BadInput("id is required")
	}
	note, err := req.Deps.Store.KbNoteByID(ctx, in.ID)
	if err != nil {
		return nil, err
	}
	body, err := os.ReadFile(note.Path)
	if err != nil {
		return nil, err
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "note": note, "body": string(body)})
}

type kbNewTool struct{}

func (kbNewTool) Name() string { return "forge_kb_new" }
func (kbNewTool) Description() string {
	return "Create a kb note (the only way agents write notes) and index it immediately; returns the new id and path."
}
func (kbNewTool) Where() string { return WhereDaemon }
func (kbNewTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"title":{"type":"string"},
		"type":{"type":"string","enum":["retro","hypothesis","proposal","spec","note"],"description":"default note"},
		"tags":{"type":"array","items":{"type":"string"}},
		"body":{"type":"string"},
		"links":{"type":"object","properties":{
			"about":{"type":"array","items":{"type":"string"}},
			"supersedes":{"type":"array","items":{"type":"string"}},
			"evidence_for":{"type":"array","items":{"type":"string"}}
		},"additionalProperties":false}
	},"required":["title"],"additionalProperties":false}`)
}

func (kbNewTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		Title string   `json:"title"`
		Type  string   `json:"type"`
		Tags  []string `json:"tags"`
		Body  string   `json:"body"`
		Links struct {
			About       []string `json:"about"`
			Supersedes  []string `json:"supersedes"`
			EvidenceFor []string `json:"evidence_for"`
		} `json:"links"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if in.Title == "" {
		return nil, BadInput("title is required")
	}
	links := map[string][]string{}
	for lt, targets := range map[string][]string{"about": in.Links.About, "supersedes": in.Links.Supersedes, "evidence_for": in.Links.EvidenceFor} {
		if len(targets) > 0 {
			links[lt] = targets
		}
	}
	// WriteNew validates type, id grammar, link grammar, and collisions before
	// touching disk; everything it refuses is the caller's input.
	n, err := kb.WriteNew(req.Deps.KbDir, kb.New{Title: in.Title, Type: in.Type, Tags: in.Tags, Links: links, Body: in.Body}, req.Deps.Clock().UTC())
	if err != nil {
		return nil, BadInput("%v", err)
	}
	// Index just this note: ReindexKb with a partial list would drop every
	// other note from the index, so IndexKbNote exists for exactly this case.
	err = req.Deps.Write(ctx, func(tx *store.Tx) error {
		if err := tx.IndexKbNote(ctx, n); err != nil {
			return err
		}
		return tx.Journal(ctx, "kb.note_created", "kb", n.ID, map[string]string{"attempt_id": req.AttemptID, "path": n.Path})
	})
	if err != nil {
		return nil, err
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "id": n.ID, "path": n.Path})
}

type kbBacklinksTool struct{}

func (kbBacklinksTool) Name() string { return "forge_kb_backlinks" }
func (kbBacklinksTool) Description() string {
	return "Notes linking to a target — a note id or a fact ref like attempt:<id> or routine:<name>."
}
func (kbBacklinksTool) Where() string { return WhereDaemon }
func (kbBacklinksTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{"ref":{"type":"string","description":"note id or fact ref"},` + pageProps + `},"required":["ref"],"additionalProperties":false}`)
}

func (kbBacklinksTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		Ref string `json:"ref"`
		page
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if err := in.clamp(); err != nil {
		return nil, err
	}
	if in.Ref == "" {
		return nil, BadInput("ref is required")
	}
	ref, err := kb.ParseRef(in.Ref)
	if err != nil {
		return nil, BadInput("%v", err)
	}
	links, err := req.Deps.Store.KbBacklinks(ctx, ref)
	if err != nil {
		return nil, err
	}
	links = slicePage(links, in.page)
	return respond(map[string]any{"schema_version": SchemaVersion, "backlinks": links, "count": len(links)})
}

type kbLinksTool struct{}

func (kbLinksTool) Name() string { return "forge_kb_links" }
func (kbLinksTool) Description() string {
	return "A note's outgoing links (frontmatter and inline), ordered by link type and target."
}
func (kbLinksTool) Where() string { return WhereDaemon }
func (kbLinksTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{"id":{"type":"string"},` + pageProps + `},"required":["id"],"additionalProperties":false}`)
}

func (kbLinksTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		ID string `json:"id"`
		page
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if err := in.clamp(); err != nil {
		return nil, err
	}
	if in.ID == "" {
		return nil, BadInput("id is required")
	}
	links, err := req.Deps.Store.KbLinks(ctx, in.ID)
	if err != nil {
		return nil, err
	}
	links = slicePage(links, in.page)
	return respond(map[string]any{"schema_version": SchemaVersion, "links": links, "count": len(links)})
}
