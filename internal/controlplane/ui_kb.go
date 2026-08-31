package controlplane

import (
	"html"
	"html/template"
	"net/http"
	"regexp"
	"sort"
	"strings"

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
	counts := map[string]int{}
	all, err := u.store.ListKbNotes(ctx, "")
	if err != nil {
		u.fail(w, r, err)
		return
	}
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
		Note: note, BodyHTML: template.HTML(renderKbMarkdown(note.Body)), Backlinks: backlinks,
	})
}

var (
	reHeading  = regexp.MustCompile(`(?m)^(#{1,4})\s+(.*)$`)
	reFence    = regexp.MustCompile("(?s)```.*?```")
	reInline   = regexp.MustCompile("`([^`]+)`")
	reBold     = regexp.MustCompile(`\*\*([^*]+)\*\*`)
	reWikiLink = regexp.MustCompile(`\[\[([a-z0-9-]+)\]\]`)
	reBullet   = regexp.MustCompile(`(?m)^[-*]\s+(.*)$`)
)

// renderKbMarkdown is a deliberately small, safe markdown renderer for kb
// note bodies: it escapes everything first, then reintroduces only a fixed
// set of tags. Not a full CommonMark implementation — headings, fenced and
// inline code, bold, [[wiki links]], bullet lists, and paragraphs, which is
// all the note format uses. [[id]] links to the note's own page.
func renderKbMarkdown(body string) string {
	// Pull fenced code out first so its contents are not reformatted.
	type block struct{ code string }
	var blocks []block
	escaped := reFence.ReplaceAllStringFunc(body, func(m string) string {
		inner := strings.TrimSuffix(strings.TrimPrefix(m, "```"), "```")
		inner = strings.TrimPrefix(inner, "\n")
		blocks = append(blocks, block{code: html.EscapeString(inner)})
		return "\x00FENCE" + itoa(len(blocks)-1) + "\x00"
	})
	escaped = html.EscapeString(escaped)

	escaped = reHeading.ReplaceAllStringFunc(escaped, func(m string) string {
		g := reHeading.FindStringSubmatch(m)
		level := len(g[1])
		if level < 2 {
			level = 2 // page already has an h1 title
		}
		return "\x00H" + itoa(level) + "\x00" + g[2] + "\x00/H\x00"
	})
	escaped = reInline.ReplaceAllString(escaped, "\x00CODE\x00$1\x00/CODE\x00")
	escaped = reBold.ReplaceAllString(escaped, "\x00B\x00$1\x00/B\x00")
	escaped = reWikiLink.ReplaceAllString(escaped, `<a href="/kb/$1">$1</a>`)

	var out strings.Builder
	inList := false
	for _, para := range strings.Split(escaped, "\n\n") {
		para = strings.TrimSpace(para)
		if para == "" {
			continue
		}
		if strings.HasPrefix(para, "\x00FENCE") {
			idx := strings.TrimSuffix(strings.TrimPrefix(para, "\x00FENCE"), "\x00")
			if n := atoi(idx); n >= 0 && n < len(blocks) {
				out.WriteString("<pre class=\"kb-code\">" + blocks[n].code + "</pre>")
			}
			continue
		}
		if reBullet.MatchString(para) {
			out.WriteString("<ul class=\"kb-list\">")
			for _, line := range strings.Split(para, "\n") {
				if g := reBullet.FindStringSubmatch(line); g != nil {
					out.WriteString("<li>" + g[1] + "</li>")
				}
			}
			out.WriteString("</ul>")
			inList = true
			continue
		}
		_ = inList
		// Headings become their own blocks.
		if strings.HasPrefix(para, "\x00H") {
			out.WriteString(para)
			continue
		}
		out.WriteString("<p>" + strings.ReplaceAll(para, "\n", "<br>") + "</p>")
	}
	s := out.String()
	// Swap the placeholder tokens for real, safe tags.
	repl := strings.NewReplacer(
		"\x00H2\x00", "<h2>", "\x00H3\x00", "<h3>", "\x00H4\x00", "<h4>", "\x00/H\x00", "</h>",
		"\x00CODE\x00", "<code>", "\x00/CODE\x00", "</code>",
		"\x00B\x00", "<strong>", "\x00/B\x00", "</strong>",
	)
	s = repl.Replace(s)
	// Close the generic </h> against the right level.
	s = regexp.MustCompile(`<h([234])>(.*?)</h>`).ReplaceAllString(s, "<h$1>$2</h$1>")
	return s
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	var b [20]byte
	i := len(b)
	for n > 0 {
		i--
		b[i] = byte('0' + n%10)
		n /= 10
	}
	return string(b[i:])
}

func atoi(s string) int {
	n := 0
	for _, c := range s {
		if c < '0' || c > '9' {
			return -1
		}
		n = n*10 + int(c-'0')
	}
	return n
}
