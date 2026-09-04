package directives

// Library search: in-memory term scoring over the hot-swapped tree. The
// library is tens of files reloaded every 30s — an index would be
// maintenance without benefit (contrast the kb, which is thousands of
// growing rows on SQLite FTS). Workflows live in SQLite; the API layer
// merges them into results with the same scoring so one ranking serves the
// UI filter, the CLI, and the forge_library agent tool.

import (
	"sort"
	"strings"
)

// SearchHit is one match: enough for a list row and for an agent to decide
// whether to fetch the full content (and whether it can call the thing).
type SearchHit struct {
	Name        string `json:"name"`
	Kind        string `json:"kind"` // directive | persona | fragment | script | workflow
	Description string `json:"description,omitempty"`
	Tool        bool   `json:"tool,omitempty"`
	InputSchema string `json:"input_schema,omitempty"` // tool-flagged scripts
	Score       int    `json:"score"`
}

// Kind names a fragment's kind for search results and the library API.
func (f *Fragment) Kind() string {
	switch {
	case f.Persona:
		return "persona"
	case f.Directive:
		return "directive"
	case f.Script:
		return "script"
	}
	return "fragment"
}

// Search matches every query term (lowercased, whitespace-split) against
// name, description, and body; all terms must hit somewhere. Score: 3 per
// term matching the name, 2 the description, 1 the body; ties break by
// name. kinds nil/empty = all kinds. An empty query lists everything
// (score 0, name order) — the browse case.
func (l *Library) Search(query string, kinds map[string]bool, limit int) []SearchHit {
	terms := strings.Fields(strings.ToLower(query))
	var hits []SearchHit
	for _, f := range l.Fragments() {
		if len(kinds) > 0 && !kinds[f.Kind()] {
			continue
		}
		score, ok := ScoreTerms(terms, f.Name, f.Description, f.Body)
		if !ok {
			continue
		}
		hit := SearchHit{Name: f.Name, Kind: f.Kind(), Description: f.Description, Tool: f.Tool, Score: score}
		if f.Script && f.Tool {
			hit.InputSchema = f.InputSchema
		}
		hits = append(hits, hit)
	}
	SortHits(hits)
	if limit > 0 && len(hits) > limit {
		hits = hits[:limit]
	}
	return hits
}

// ScoreTerms is the shared scoring: every term must match name (3),
// description (2), or body (1); the best field per term counts. Exported so
// the API layer scores workflow rows identically.
func ScoreTerms(terms []string, name, description, body string) (int, bool) {
	if len(terms) == 0 {
		return 0, true
	}
	lname, ldesc, lbody := strings.ToLower(name), strings.ToLower(description), strings.ToLower(body)
	score := 0
	for _, t := range terms {
		switch {
		case strings.Contains(lname, t):
			score += 3
		case strings.Contains(ldesc, t):
			score += 2
		case strings.Contains(lbody, t):
			score++
		default:
			return 0, false
		}
	}
	return score, true
}

// SortHits orders by score descending, then name.
func SortHits(hits []SearchHit) {
	sort.Slice(hits, func(i, j int) bool {
		if hits[i].Score != hits[j].Score {
			return hits[i].Score > hits[j].Score
		}
		return hits[i].Name < hits[j].Name
	})
}
