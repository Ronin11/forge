package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"
)

// Plugin is one row of the plugins table (DESIGN.md §17): what is installed,
// whether a human enabled it, the hash of its minted token, and its journal
// cursor.
type Plugin struct {
	Name        string    `json:"name"`
	Version     string    `json:"version"`
	Kind        string    `json:"kind"` // first_party | third_party
	Path        string    `json:"path"`
	Enabled     bool      `json:"enabled"`
	Scopes      []string  `json:"scopes"`
	TokenHash   string    `json:"-"`
	Cursor      int64     `json:"cursor"`
	InstalledAt time.Time `json:"installed_at"`
	EnabledAt   time.Time `json:"enabled_at,omitempty"`
}

// InstallPlugin records (or refreshes) an installed plugin. Enabled state,
// token, and cursor survive a reinstall of a newer version.
func (tx *Tx) InstallPlugin(ctx context.Context, p Plugin) error {
	scopes, err := json.Marshal(p.Scopes)
	if err != nil {
		return fmt.Errorf("marshal scopes: %w", err)
	}
	if _, err := tx.Exec(ctx, `INSERT INTO plugins (name, version, kind, path, enabled, scopes, installed_at) VALUES (?, ?, ?, ?, 0, ?, ?)
		ON CONFLICT(name) DO UPDATE SET version = excluded.version, kind = excluded.kind, path = excluded.path, scopes = excluded.scopes`,
		p.Name, p.Version, p.Kind, p.Path, string(scopes), formatTime(tx.now)); err != nil {
		return fmt.Errorf("install plugin %s: %w", p.Name, err)
	}
	return tx.Journal(ctx, "plugin.installed", EntityPlugin, p.Name, map[string]any{"version": p.Version, "kind": p.Kind})
}

// UninstallPlugin removes the row entirely.
func (tx *Tx) UninstallPlugin(ctx context.Context, name string) error {
	res, err := tx.Exec(ctx, `DELETE FROM plugins WHERE name = ?`, name)
	if err != nil {
		return fmt.Errorf("uninstall plugin %s: %w", name, err)
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return fmt.Errorf("plugin %s: %w", name, ErrNotFound)
	}
	return tx.Journal(ctx, "plugin.uninstalled", EntityPlugin, name, nil)
}

// EnablePlugin marks the plugin enabled with the minted token's hash — the
// human approval of its scopes happened in the CLI.
func (tx *Tx) EnablePlugin(ctx context.Context, name, tokenHash string) (*Plugin, error) {
	if _, err := tx.Exec(ctx, `UPDATE plugins SET enabled = 1, token_hash = ?, enabled_at = ? WHERE name = ?`,
		tokenHash, formatTime(tx.now), name); err != nil {
		return nil, fmt.Errorf("enable plugin %s: %w", name, err)
	}
	p, err := tx.getPlugin(ctx, name)
	if err != nil {
		return nil, err
	}
	if err := tx.Journal(ctx, "plugin.enabled", EntityPlugin, name, map[string]any{"scopes": p.Scopes}); err != nil {
		return nil, err
	}
	return p, nil
}

// DisablePlugin clears enabled and revokes the token.
func (tx *Tx) DisablePlugin(ctx context.Context, name string) error {
	res, err := tx.Exec(ctx, `UPDATE plugins SET enabled = 0, token_hash = NULL WHERE name = ?`, name)
	if err != nil {
		return fmt.Errorf("disable plugin %s: %w", name, err)
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return fmt.Errorf("plugin %s: %w", name, ErrNotFound)
	}
	return tx.Journal(ctx, "plugin.disabled", EntityPlugin, name, nil)
}

// AckPluginCursor persists the last journal id the plugin acknowledged, so a
// restart resumes with no gaps and no duplicates beyond it. High-frequency:
// deliberately not journaled.
func (tx *Tx) AckPluginCursor(ctx context.Context, name string, cursor int64) error {
	res, err := tx.Exec(ctx, `UPDATE plugins SET cursor = ? WHERE name = ? AND (cursor IS NULL OR cursor <= ?)`, cursor, name, cursor)
	if err != nil {
		return fmt.Errorf("ack plugin %s cursor: %w", name, err)
	}
	if n, _ := res.RowsAffected(); n == 0 {
		// Either unknown, or a stale ack below the stored cursor; distinguish.
		if _, gerr := tx.getPlugin(ctx, name); gerr != nil {
			return gerr
		}
	}
	return nil
}

// Plugins lists every installed plugin sorted by name.
func (s *Store) Plugins(ctx context.Context) ([]Plugin, error) {
	return scanPlugins(each(s.query(ctx, `SELECT `+pluginColumns+` FROM plugins ORDER BY name`)))
}

// GetPlugin reads one plugin by name.
func (s *Store) GetPlugin(ctx context.Context, name string) (*Plugin, error) {
	ps, err := scanPlugins(each(s.query(ctx, `SELECT `+pluginColumns+` FROM plugins WHERE name = ?`, name)))
	if err != nil {
		return nil, err
	}
	if len(ps) == 0 {
		return nil, fmt.Errorf("plugin %s: %w", name, ErrNotFound)
	}
	return &ps[0], nil
}

func (tx *Tx) getPlugin(ctx context.Context, name string) (*Plugin, error) {
	ps, err := scanPlugins(each(tx.Query(ctx, `SELECT `+pluginColumns+` FROM plugins WHERE name = ?`, name)))
	if err != nil {
		return nil, err
	}
	if len(ps) == 0 {
		return nil, fmt.Errorf("plugin %s: %w", name, ErrNotFound)
	}
	return &ps[0], nil
}

// PluginByTokenHash resolves a presented bearer token (already hashed) to the
// enabled plugin carrying it; the scope check is the caller's.
func (s *Store) PluginByTokenHash(ctx context.Context, hash string) (*Plugin, error) {
	ps, err := scanPlugins(each(s.query(ctx, `SELECT `+pluginColumns+` FROM plugins WHERE token_hash = ? AND enabled = 1`, hash)))
	if err != nil {
		return nil, err
	}
	if len(ps) == 0 {
		return nil, fmt.Errorf("plugin token: %w", ErrNotFound)
	}
	return &ps[0], nil
}

const pluginColumns = `name, version, kind, path, enabled, scopes, token_hash, cursor, installed_at, enabled_at`

func scanPlugins(iter func(func(*sql.Rows) error) error) ([]Plugin, error) {
	var out []Plugin
	err := iter(func(rows *sql.Rows) error {
		var p Plugin
		var enabled int
		var scopes string
		var tokenHash sql.NullString
		var cursor sql.NullInt64
		var installed, enabledAt sql.NullString
		if err := rows.Scan(&p.Name, &p.Version, &p.Kind, &p.Path, &enabled, &scopes, &tokenHash, &cursor, &installed, &enabledAt); err != nil {
			return err
		}
		p.Enabled = enabled != 0
		if err := json.Unmarshal([]byte(scopes), &p.Scopes); err != nil {
			return fmt.Errorf("plugin %s scopes: %w", p.Name, err)
		}
		p.TokenHash, p.Cursor = tokenHash.String, cursor.Int64
		var err error
		if p.InstalledAt, err = parseTime(installed); err != nil {
			return err
		}
		if p.EnabledAt, err = parseTime(enabledAt); err != nil {
			return err
		}
		out = append(out, p)
		return nil
	})
	return out, err
}
