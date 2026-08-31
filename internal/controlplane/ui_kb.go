package controlplane

import (
	"bytes"
	"html/template"
	"net/http"
	"regexp"
	"sort"
	"strings"

	"github.com/microcosm-cc/bluemonday"
	"github.com/yuin/goldmark"
	"github.com/yuin/goldmark/extension"
	gmhtml "github.com/yuin/goldmark/renderer/html"

	"forge/internal/kb"
	"forge/internal/store"
)

// kb is GET /kb: the Knowledge page — every note, or a full-text search when
// ?q= is given, with the tag filter ?tag=.
func (u *UI) kb(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	query := strings.TrimSpace(r.URL.Query().Get("q"))
	tag := strings.TrimSpace(r.URL.Query().Get("tag"))
	var notes []store.KbNote
	var err error
	if query != "" {
		notes, err = u.store.SearchKb(ctx, query, 100)
	} else {
		notes, err = u.store.ListKbNotes(ctx, tag)
	}
	if err != nil {
		u.fail(w, r, err)
		return
	}
	// The tag rail: every tag in use, with counts, for one-click filtering.
	all, err := u.store.ListKbNotes(ctx, "")
	if err != nil {
		u.fail(w, r, err)
		return
	}
	counts := map[string]int{}
	for _, n := range all {
		for _, t := range n.Tags {
			counts[t]++
		}
	}
	type tagCount struct {
		Tag   string
		Count int
	}
	tags := make([]tagCount, 0, len(counts))
	for t, c := range counts {
		tags = append(tags, tagCount{t, c})
	}
	sort.Slice(tags, func(i, j int) bool {
		if tags[i].Count != tags[j].Count {
			return tags[i].Count > tags[j].Count
		}
		return tags[i].Tag < tags[j].Tag
	})
	u.render(w, r, "kb.html", "Knowledge", map[string]any{
		"Notes": notes, "Query": query, "Tag": tag, "Tags": tags, "Total": len(all),
	})
}

// kbNoteView is one note rendered for reading.
type kbNoteView struct {
	Note      *kb.Note
	BodyHTML  template.HTML
	Backlinks []store.KbLink
}

// kbNote is GET /kb/{id}: one note, its markdown body rendered, with the notes
// that link to it.
func (u *UI) kbNote(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	id := r.PathValue("id")
	meta, err := u.store.KbNoteByID(ctx, id)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	note, err := kb.Parse(meta.Path)
	if err != nil {
		u.fail(w, r, err)
		return
	}
	backlinks, err := u.store.KbBacklinks(ctx, kb.Ref{Kind: "note", Val: id})
	if err != nil {
		u.fail(w, r, err)
		return
	}
	u.render(w, r, "kbnote.html", note.Title, kbNoteView{
		Note: note, BodyHTML: renderKbMarkdown(note.Body), Backlinks: backlinks,
	})
}

// reWikiLink turns the note format's [[id]] links into ordinary Markdown links
// to the note's page before goldmark ever sees them (goldmark has no wiki-link
// syntax). Ids are the restricted [a-z0-9-] slug kb.WriteNew produces.
var reWikiLink = regexp.MustCompile(`\[\[([a-z0-9-]+)\]\]`)

// mdRenderer is goldmark with GitHub-flavoured Markdown (tables, strikethrough,
// task lists, autolinks). Raw HTML in a note is escaped, not passed through
// (WithUnsafe is deliberately off); bluemonday then sanitises the output, so a
// note built from untrusted repository content (constitution 9) cannot inject
// script or a javascript: URL.
var mdRenderer = goldmark.New(
	goldmark.WithExtensions(extension.GFM),
	goldmark.WithRendererOptions(gmhtml.WithHardWraps()),
)

// mdPolicy allows the tags GFM emits and nothing dangerous; relative URLs stay
// so /kb/<id> links resolve.
var mdPolicy = func() *bluemonday.Policy {
	p := bluemonday.UGCPolicy()
	p.AllowRelativeURLs(true)
	p.RequireNoReferrerOnLinks(true)
	p.AllowAttrs("class").OnElements("code", "span", "pre", "input", "li", "ul")
	p.AllowAttrs("type", "checked", "disabled").OnElements("input") // GFM task lists
	return p
}()

// renderKbMarkdown renders a kb note body to safe HTML: [[wiki links]] →
// /kb/<id>, then goldmark (GFM), then bluemonday. Replaces the earlier
// hand-rolled renderer.
func renderKbMarkdown(body string) template.HTML {
	pre := reWikiLink.ReplaceAllString(body, `[$1](/kb/$1)`)
	var buf bytes.Buffer
	if err := mdRenderer.Convert([]byte(pre), &buf); err != nil {
		// Convert only errors on a broken writer; fall back to escaped text.
		return template.HTML("<pre>" + template.HTMLEscapeString(body) + "</pre>")
	}
	return template.HTML(mdPolicy.SanitizeBytes(buf.Bytes()))
}
