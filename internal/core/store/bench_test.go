package store

import (
	"context"
	"fmt"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// BenchmarkInsertEvents is the event-ingestion path: one 100-event batch per
// iteration in one transaction with a prepared statement. bench/threshold.txt
// records the ceiling `just bench` enforces.
func BenchmarkInsertEvents(b *testing.B) {
	f := newFixture(b)
	_, target := f.newWork(model.ClassNormal)
	a := f.claim(target, "bench")
	batch := make([]protocol.Event, 100)
	for i := range batch {
		batch[i] = protocol.Event{Seq: i, Time: f.now, ElapsedUS: int64(i * 1000), Kind: protocol.KindSpanStart, SpanID: fmt.Sprintf("s%d", i), Name: "Bash", Attrs: []byte(`{"tool":"Bash","input_bytes":42}`)}
	}
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		for j := range batch {
			batch[j].Seq = i*100 + j
		}
		if err := f.s.Write(context.Background(), func(tx *Tx) error {
			_, err := tx.InsertEvents(context.Background(), a.ID, protocol.SourceWorker, batch)
			return err
		}); err != nil {
			b.Fatal(err)
		}
	}
}
