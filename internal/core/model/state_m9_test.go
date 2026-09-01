package model

import "testing"

// HoldsWriteSet is the lease lifetime rule (M9): exactly the four states in
// which an attempt's write set is live.
func TestHoldsWriteSet(t *testing.T) {
	holds := map[State]bool{Claimed: true, Preparing: true, Running: true, Verifying: true}
	for _, s := range []State{Pending, Claimed, Preparing, Running, WaitingHuman, Verifying, Succeeded, Unverified, Failed, Cancelled, QueuedForMerge, Merging, Merged, Conflict} {
		if got := HoldsWriteSet(s); got != holds[s] {
			t.Errorf("HoldsWriteSet(%s) = %v, want %v", s, got, holds[s])
		}
	}
}
