package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"forge/internal/kb"
)

// KbNote is one indexed note row.
type KbNote struct {
	ID      string    `json:"id"`
	Path    string    `json:"path"`
	Title   string    `json:"title"`
	Type    string    `json:"type"`
	Created time.Time `json:"created"`
	Tags    []string  `json:"tags"`
}

// KbLink is one indexed link.
type KbLink struct {
	FromID   string `json:"from_id"`
	ToKind   string `json:"to_kind"` // note | attempt | work | …
	ToRef    string `json:"to_ref"`
	LinkType string `json:"link_type"` // inline | about | supersedes | evidence_for
}

// ReindexKb brings the index in step with the scanned notes: rows whose
// (mtime, size) changed are re-written, vanished files are dropped. It returns
// how many notes were (re)indexed. The files are the truth; this is derived.
func (tx *Tx) ReindexKb(ctx context.Context, notes []*kb.Note) (int, error) {
	existing := map[string][2]int64{}
	err := each(tx.Query(ctx, `SELECT id, mtime, hash FROM kb_notes`))(func(rows *sql.Rows) error {
		var id, hash string
		var mtime int64
		if err := rows.Scan(&id, &mtime, &hash); err != nil {
			return err
		}
		var size int64
		if _, err := fmt.Sscanf(hash, "size:%d", &size); err != nil {
			size = -1
		}
		existing[id] = [2]int64{mtime, size}
		return nil
	})
	if err != nil {
		return 0, fmt.Errorf("read kb index: %w", err)
	}
	seen := map[string]bool{}
	indexed := 0
	for _, n := range notes {
		seen[n.ID] = true
		if prev, ok := existing[n.ID]; ok && prev[0] == n.ModTime.UnixNano() && prev[1] == n.Size {
			continue
		}
		if err := tx.indexNote(ctx, n); err != nil {
			return indexed, err
		}
		indexed++
	}
	for id := range existing {
		if !seen[id] {
			if err := tx.dropNote(ctx, id); err != nil {
				return indexed, err
			}
		}
	}
	return indexed, nil
}

func (tx *Tx) indexNote(ctx context.Context, n *kb.Note) error {
	tags, err := json.Marshal(n.Tags)
	if err != nil {
		return fmt.Errorf("marshal tags of %s: %w", n.ID, err)
	}
	if err := tx.dropNote(ctx, n.ID); err != nil {
		return err
	}
	if _, err := tx.Exec(ctx, `INSERT INTO kb_notes (id, path, title, type, created, tags, mtime, hash, body_hash, indexed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, '', ?)`,
		n.ID, n.Path, n.Title, n.Type, formatTime(n.Created), string(tags), n.ModTime.UnixNano(), fmt.Sprintf("size:%d", n.Size), formatTime(tx.now)); err != nil {
		return fmt.Errorf("index note %s: %w", n.ID, err)
	}
	if _, err := tx.Exec(ctx, `INSERT INTO kb_fts (id, title, body) VALUES (?, ?, ?)`, n.ID, n.Title, n.Body); err != nil {
		return fmt.Errorf("index note text %s: %w", n.ID, err)
	}
	insert := func(linkType string, targets []string) error {
		for _, t := range targets {
			ref, err := kb.ParseRef(t)
			if err != nil {
				// Scan accepted the note; a malformed link is check's finding,
				// not an index failure. Index it as unresolvable text.
				ref = kb.Ref{Kind: "invalid", Val: t}
			}
			if _, err := tx.Exec(ctx, `INSERT OR IGNORE INTO kb_links (from_id, to_kind, to_ref, link_type) VALUES (?, ?, ?, ?)`, n.ID, ref.Kind, ref.Val, linkType); err != nil {
				return fmt.Errorf("index link of %s: %w", n.ID, err)
			}
		}
		return nil
	}
	if err := insert("inline", n.Inline); err != nil {
		return err
	}
	for lt, targets := range n.Links {
		if err := insert(lt, targets); err != nil {
			return err
		}
	}
	return nil
}

func (tx *Tx) dropNote(ctx context.Context, id string) error {
	for _, q := range []string{`DELETE FROM kb_links WHERE from_id = ?`, `DELETE FROM kb_fts WHERE id = ?`, `DELETE FROM kb_notes WHERE id = ?`} {
		if _, err := tx.Exec(ctx, q, id); err != nil {
			return fmt.Errorf("drop note %s: %w", id, err)
		}
	}
	return nil
}

// SearchKb is FTS over titles and bodies, newest first on ties.
func (s *Store) SearchKb(ctx context.Context, query string, limit int) ([]KbNote, error) {
	if limit <= 0 || limit > 100 {
		limit = 20
	}
	// Quote each term so user input cannot inject FTS syntax.
	terms := strings.Fields(query)
	for i, t := range terms {
		terms[i] = `"` + strings.ReplaceAll(t, `"`, ``) + `"`
	}
	if len(terms) == 0 {
		return nil, nil
	}
	return s.scanKbNotes(each(s.query(ctx, `SELECT n.id, n.path, n.title, n.type, n.created, n.tags FROM kb_fts f JOIN kb_notes n ON n.id = f.id WHERE kb_fts MATCH ? ORDER BY rank LIMIT ?`, strings.Join(terms, " "), limit)))
}

// KbBacklinks lists notes linking to a target (note id or fact ref).
func (s *Store) KbBacklinks(ctx context.Context, ref kb.Ref) ([]KbLink, error) {
	var out []KbLink
	err := each(s.query(ctx, `SELECT from_id, to_kind, to_ref, link_type FROM kb_links WHERE to_kind = ? AND to_ref = ? ORDER BY from_id, link_type`, ref.Kind, ref.Val))(func(rows *sql.Rows) error {
		var l KbLink
		if err := rows.Scan(&l.FromID, &l.ToKind, &l.ToRef, &l.LinkType); err != nil {
			return err
		}
		out = append(out, l)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read backlinks: %w", err)
	}
	return out, nil
}

// KbLinks lists a note's outgoing links.
func (s *Store) KbLinks(ctx context.Context, id string) ([]KbLink, error) {
	var out []KbLink
	err := each(s.query(ctx, `SELECT from_id, to_kind, to_ref, link_type FROM kb_links WHERE from_id = ? ORDER BY link_type, to_kind, to_ref`, id))(func(rows *sql.Rows) error {
		var l KbLink
		if err := rows.Scan(&l.FromID, &l.ToKind, &l.ToRef, &l.LinkType); err != nil {
			return err
		}
		out = append(out, l)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read links of %s: %w", id, err)
	}
	return out, nil
}

// KbNoteByID returns one indexed note, or ErrNotFound.
func (s *Store) KbNoteByID(ctx context.Context, id string) (*KbNote, error) {
	ns, err := s.scanKbNotes(each(s.query(ctx, `SELECT id, path, title, type, created, tags FROM kb_notes WHERE id = ?`, id)))
	if err != nil {
		return nil, err
	}
	if len(ns) == 0 {
		return nil, fmt.Errorf("note %s: %w", id, ErrNotFound)
	}
	return &ns[0], nil
}

// KbFactExists resolves a fact reference against the tables — the daemon side
// of kb.FactChecker.
func (s *Store) KbFactExists(ctx context.Context, ref kb.Ref) (bool, error) {
	var q string
	val := ref.Val
	switch ref.Kind {
	case "note":
		q = `SELECT count(*) FROM kb_notes WHERE id = ?`
	case "attempt":
		q = `SELECT count(*) FROM attempts WHERE id = ?`
	case "work":
		q = `SELECT count(*) FROM work WHERE id = ?`
	case "target":
		q = `SELECT count(*) FROM targets WHERE id = ?`
	case "proposal":
		q = `SELECT count(*) FROM proposals WHERE id = ?`
	case "repository":
		q = `SELECT count(*) FROM repositories WHERE name = ?`
	case "project":
		q = `SELECT count(*) FROM projects WHERE name = ?`
	case "prompt":
		q = `SELECT count(*) FROM prompt_versions WHERE hash = ?`
	case "routine":
		name, gen, ok := strings.Cut(ref.Val, "@")
		if !ok {
			q, val = `SELECT count(*) FROM routines WHERE name = ?`, name
			break
		}
		var n int
		err := s.queryRow(ctx, `SELECT count(*) FROM routine_generations g JOIN routines r ON r.id = g.routine_id WHERE r.name = ? AND g.generation = ?`, name, gen).Scan(&n)
		return n > 0, err
	default:
		return false, fmt.Errorf("unknown fact kind %q", ref.Kind)
	}
	var n int
	if err := s.queryRow(ctx, q, val).Scan(&n); err != nil {
		return false, fmt.Errorf("resolve %s:%s: %w", ref.Kind, ref.Val, err)
	}
	return n > 0, nil
}

func (s *Store) scanKbNotes(iter func(func(*sql.Rows) error) error) ([]KbNote, error) {
	var out []KbNote
	err := iter(func(rows *sql.Rows) error {
		var n KbNote
		var created, tags string
		if err := rows.Scan(&n.ID, &n.Path, &n.Title, &n.Type, &created, &tags); err != nil {
			return err
		}
		var err error
		if n.Created, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
			return err
		}
		if err := json.Unmarshal([]byte(tags), &n.Tags); err != nil {
			return fmt.Errorf("decode tags of %s: %w", n.ID, err)
		}
		out = append(out, n)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read kb notes: %w", err)
	}
	return out, nil
}
