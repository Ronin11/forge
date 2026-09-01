package controlplane

import (
	"bytes"
	"html/template"
	"net/http"
	"regexp"
	"slices"
	"sort"
	"strings"

	"github.com/microcosm-cc/bluemonday"
	"github.com/yuin/goldmark"
	"github.com/yuin/goldmark/extension"
	gmhtml "github.com/yuin/goldmark/renderer/html"

	"forge/internal/core/kb"
	"forge/internal/store"
)

// parseKbQuery splits a search-bar query into free text and the qualifiers the
// Knowledge page understands (tag:, type:), matching the client-side DSL: the
// last tag:/type: wins, quotes around a value are shed, everything else is the
// full-text query.
func parseKbQuery(q string) (text, tag, typ string) {
	var rest []string
	for _, tok := range strings.Fields(q) {
		key, val, ok := strings.Cut(tok, ":")
		if ok {
			val = strings.Trim(val, `"`)
		}
		switch {
		case ok && strings.EqualFold(key, "tag"):
			tag = val
		case ok && strings.EqualFold(key, "type"):
			typ = val
		default:
			rest = append(rest, tok)
		}
	}
	return strings.Join(rest, " "), tag, typ
}

// kb is GET /kb: the Knowledge page — every note, or a full-text search when
// ?q= is given. The query may carry tag:/type: qualifiers (the search-bar DSL);
// the tag rail's ?tag= keeps working and a qualifier in q wins over it.
func (u *UI) kb(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	query := strings.TrimSpace(r.URL.Query().Get("q"))
	tag := strings.TrimSpace(r.URL.Query().Get("tag"))
	text, qtag, qtype := parseKbQuery(query)
	if qtag != "" {
		tag = qtag
	}
	var notes []store.KbNote
	var err error
	if text != "" {
		notes, err = u.store.SearchKb(ctx, text, 100)
	} else {
		notes, err = u.store.ListKbNotes(ctx, tag)
	}
	if err != nil {
		u.fail(w, r, err)
		return
	}
	// Qualifier filters the list form could not express: tag on a full-text
	// result, and type always.
	if tag != "" && text != "" {
		notes = filterNotes(notes, func(n store.KbNote) bool { return slices.Contains(n.Tags, tag) })
	}
	if qtype != "" {
		notes = filterNotes(notes, func(n store.KbNote) bool { return strings.HasPrefix(n.Type, qtype) })
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
	// Suggestion values for the search bar: every tag and every note type.
	tagNames := make([]string, len(tags))
	for i, t := range tags {
		tagNames[i] = t.Tag
	}
	types := map[string]bool{}
	for _, n := range all {
		if n.Type != "" {
			types[n.Type] = true
		}
	}
	typeNames := make([]string, 0, len(types))
	for t := range types {
		typeNames = append(typeNames, t)
	}
	sort.Strings(typeNames)
	// An active ?tag= filter with no typed query shows up in the bar as its DSL
	// form, so submitting keeps it.
	if query == "" && tag != "" {
		query = "tag:" + tag
	}
	u.render(w, r, "kb.html", "Knowledge", map[string]any{
		"Notes": notes, "Query": query, "Tag": tag, "Tags": tags, "Total": len(all),
		"TagValues": strings.Join(tagNames, " "), "TypeValues": strings.Join(typeNames, " "),
	})
}

// filterNotes keeps the notes for which keep is true.
func filterNotes(notes []store.KbNote, keep func(store.KbNote) bool) []store.KbNote {
	out := notes[:0]
	for _, n := range notes {
		if keep(n) {
			out = append(out, n)
		}
	}
	return out
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
