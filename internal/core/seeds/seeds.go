// Package seeds imports the base system's starter workflows and trigger
// routines from the directives library's seeds/ directory into SQLite, so a
// first run of forge is a working system rather than empty pages. Seeds ride
// the library repo (seeds/routines/*.json, seeds/workflows/*.json — the API's
// wire shapes), so they update with the base like everything else in git.
// Import is skip-if-name-exists per row: it runs on every boot, touches
// nothing the operator has (or ever had) under the same name, and never
// resurrects something deleted — an archived row still owns its name.
package seeds

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"forge/internal/core/store"
)

// Import loads seeds/ under libDir. Workflows import before routines so a
// routine targeting a seeded workflow validates. Returns the names added.
func Import(ctx context.Context, st *store.Store, libDir string, log *slog.Logger) ([]string, error) {
	var added []string
	workflows, err := readSeeds[store.Workflow](filepath.Join(libDir, "seeds", "workflows"))
	if err != nil {
		return nil, err
	}
	for _, wf := range workflows {
		wf := wf
		ok, err := importOne(ctx, st, "workflow", wf.Name, func(tx *store.Tx) (bool, error) {
			if _, err := tx.GetWorkflow(ctx, wf.Name); err == nil {
				return false, nil
			}
			return true, tx.CreateWorkflow(ctx, &wf)
		})
		if err != nil {
			log.WarnContext(ctx, "seed workflow refused", "workflow", wf.Name, "error", err)
			continue
		}
		if ok {
			added = append(added, "workflow:"+wf.Name)
		}
	}
	routines, err := readSeeds[store.Routine](filepath.Join(libDir, "seeds", "routines"))
	if err != nil {
		return nil, err
	}
	for _, rt := range routines {
		rt := rt
		ok, err := importOne(ctx, st, "routine", rt.Name, func(tx *store.Tx) (bool, error) {
			if _, err := tx.GetRoutine(ctx, rt.Name); err == nil {
				return false, nil
			}
			return true, tx.CreateRoutine(ctx, &rt)
		})
		if err != nil {
			log.WarnContext(ctx, "seed routine refused", "routine", rt.Name, "error", err)
			continue
		}
		if ok {
			added = append(added, "routine:"+rt.Name)
		}
	}
	return added, nil
}

// importOne runs one seed's check-and-create in a transaction and journals
// the addition.
func importOne(ctx context.Context, st *store.Store, kind, name string, create func(tx *store.Tx) (bool, error)) (bool, error) {
	created := false
	err := st.Write(ctx, func(tx *store.Tx) error {
		ok, err := create(tx)
		if err != nil || !ok {
			return err
		}
		created = true
		return tx.Journal(ctx, "seed.imported", store.EntityDaemon, name, map[string]any{"kind": kind, "name": name})
	})
	return created, err
}

// readSeeds decodes every *.json in dir, sorted by filename for a stable
// import order; a missing dir is an empty set.
func readSeeds[T any](dir string) ([]T, error) {
	entries, err := os.ReadDir(dir)
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	names := make([]string, 0, len(entries))
	for _, e := range entries {
		if !e.IsDir() && strings.HasSuffix(e.Name(), ".json") {
			names = append(names, e.Name())
		}
	}
	sort.Strings(names)
	out := make([]T, 0, len(names))
	for _, name := range names {
		raw, err := os.ReadFile(filepath.Join(dir, name))
		if err != nil {
			return nil, err
		}
		var v T
		if err := json.Unmarshal(raw, &v); err != nil {
			return nil, fmt.Errorf("seed %s: %w", name, err)
		}
		out = append(out, v)
	}
	return out, nil
}
