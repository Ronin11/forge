package store

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
)

func TestGenerationSnapshotAndSource(t *testing.T) {
	s := openTest(t)
	bg := context.Background()
	r := &Routine{Name: "inventory", Target: "directive:inventory", Objective: "old", TimeoutSeconds: 300}
	if err := s.Write(bg, func(tx *Tx) error { return tx.CreateRoutine(bg, r) }); err != nil {
		t.Fatal(err)
	}
	r.Objective = "new"
	if err := s.Write(bg, func(tx *Tx) error { return tx.UpdateRoutineFrom(bg, r, 1, "proposal:abc") }); err != nil {
		t.Fatal(err)
	}
	err := s.Write(bg, func(tx *Tx) error {
		snap, err := tx.GenerationSnapshot(bg, r.ID, 1)
		if err != nil {
			return err
		}
		var old Routine
		if err := json.Unmarshal(snap, &old); err != nil {
			return err
		}
		if old.Objective != "old" || old.Generation != 1 {
			t.Errorf("generation 1 snapshot = objective %q generation %d", old.Objective, old.Generation)
		}
		// The source of the proposal-made generation is recorded verbatim.
		var source string
		if err := tx.QueryRow(bg, `SELECT source FROM routine_generations WHERE routine_id = ? AND generation = 2`, r.ID).Scan(&source); err != nil {
			return err
		}
		if source != "proposal:abc" {
			t.Errorf("generation 2 source = %q, want proposal:abc", source)
		}
		if _, err := tx.GenerationSnapshot(bg, r.ID, 9); !errors.Is(err, ErrNotFound) {
			t.Errorf("missing generation = %v, want ErrNotFound", err)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}
