package store

import (
	"encoding/json"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/protocol"
)

// usageAttrs builds the attrs a "usage" metric carries, matching what the
// worker's stream parser stamps.
func usageAttrs(t *testing.T, id string, in, out int64) json.RawMessage {
	t.Helper()
	b, err := json.Marshal(map[string]any{"message_id": id, "input_tokens": in, "output_tokens": out})
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func TestAttemptProgressTally(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassInteractive)
	a := f.claim(target, "req-1")
	f.run(a.ID, "lease-req-1")

	base := f.now.Add(30 * time.Second)

	// Two assistant turns arrive as de-duplicated usage metrics; the tally counts
	// the turns and sums the token fields.
	f.write(func(tx *Tx) error {
		evs := []protocol.Event{
			{Seq: 1, Time: base, Kind: protocol.KindMetric, Name: "usage", Message: "usage m1", Attrs: usageAttrs(t, "m1", 100, 40)},
			{Seq: 2, Time: base.Add(time.Second), Kind: protocol.KindMetric, Name: "usage", Message: "usage m2", Attrs: usageAttrs(t, "m2", 80, 112)},
			{Seq: 3, Time: base.Add(2 * time.Second), Kind: protocol.KindStdout, Message: "log line"},
		}
		if _, err := tx.InsertEvents(ctx(), a.ID, protocol.SourceWorker, evs); err != nil {
			return err
		}
		return tx.RecomputeAttemptProgress(ctx(), a.ID)
	})

	p := must(f.s.AttemptProgress(ctx(), a.ID))
	if p == nil {
		t.Fatal("no progress row after events")
	}
	if p.RunningTurns != 2 {
		t.Errorf("running turns = %d, want 2", p.RunningTurns)
	}
	if p.TokensIn != 180 || p.TokensOut != 152 {
		t.Errorf("tokens in/out = %d/%d, want 180/152", p.TokensIn, p.TokensOut)
	}
	if !p.LastEventAt.Equal(base.Add(2 * time.Second)) {
		t.Errorf("last event at = %v, want %v", p.LastEventAt, base.Add(2*time.Second))
	}

	// Redelivery of the same batch must not double count — the tally is recomputed
	// from the events table, not accumulated.
	f.write(func(tx *Tx) error {
		evs := []protocol.Event{
			{Seq: 1, Time: base, Kind: protocol.KindMetric, Name: "usage", Message: "usage m1", Attrs: usageAttrs(t, "m1", 100, 40)},
			{Seq: 2, Time: base.Add(time.Second), Kind: protocol.KindMetric, Name: "usage", Message: "usage m2", Attrs: usageAttrs(t, "m2", 80, 112)},
		}
		if _, err := tx.InsertEvents(ctx(), a.ID, protocol.SourceWorker, evs); err != nil {
			return err
		}
		return tx.RecomputeAttemptProgress(ctx(), a.ID)
	})
	p = must(f.s.AttemptProgress(ctx(), a.ID))
	if p.RunningTurns != 2 || p.TokensIn != 180 || p.TokensOut != 152 {
		t.Errorf("after redelivery: turns %d tokens %d/%d, want 2 180/152", p.RunningTurns, p.TokensIn, p.TokensOut)
	}

	// A forge_note_progress note (a lifecycle "progress" event) surfaces as the
	// latest note with its checkpoint.
	noteAt := base.Add(10 * time.Second)
	f.write(func(tx *Tx) error {
		attrs, err := json.Marshal(map[string]string{"checkpoint": "after_plan"})
		if err != nil {
			return err
		}
		ev := protocol.Event{Seq: 1, Time: noteAt, Kind: protocol.KindLifecycle, Name: "progress", Message: "plan complete, building", Attrs: attrs}
		if _, err := tx.InsertEvents(ctx(), a.ID, protocol.SourceControl, []protocol.Event{ev}); err != nil {
			return err
		}
		return tx.RecomputeAttemptProgress(ctx(), a.ID)
	})
	p = must(f.s.AttemptProgress(ctx(), a.ID))
	if p.Note != "plan complete, building" || p.Checkpoint != "after_plan" {
		t.Errorf("note/checkpoint = %q/%q", p.Note, p.Checkpoint)
	}
	if !p.NoteAt.Equal(noteAt) {
		t.Errorf("note at = %v, want %v", p.NoteAt, noteAt)
	}
	// The note must not disturb the turn/token tally.
	if p.RunningTurns != 2 || p.TokensIn != 180 {
		t.Errorf("after note: turns %d tokens in %d", p.RunningTurns, p.TokensIn)
	}

	// A heartbeat records the reported state and phase without touching the tally.
	f.write(func(tx *Tx) error {
		return tx.RecordHeartbeatProgress(ctx(), a.ID, model.Running, "build")
	})
	p = must(f.s.AttemptProgress(ctx(), a.ID))
	if p.State != model.Running || p.Phase != "build" {
		t.Errorf("state/phase = %q/%q, want running/build", p.State, p.Phase)
	}
	if p.PhaseAt.IsZero() {
		t.Error("phase_at not stamped by heartbeat")
	}
	if p.RunningTurns != 2 || p.Note != "plan complete, building" {
		t.Errorf("heartbeat disturbed tally/note: turns %d note %q", p.RunningTurns, p.Note)
	}
}

func TestAttemptProgressAbsent(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassInteractive)
	a := f.claim(target, "req-1")
	// Nothing has arrived yet: no progress row.
	if p := must(f.s.AttemptProgress(ctx(), a.ID)); p != nil {
		t.Errorf("expected nil progress, got %+v", p)
	}
}
