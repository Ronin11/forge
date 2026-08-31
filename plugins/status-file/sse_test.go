package main

import (
	"errors"
	"strings"
	"testing"
)

func TestParseSSEFrames(t *testing.T) {
	stream := ": keepalive\n" +
		"\n" +
		"event: journal\n" +
		"id: 41\n" +
		"data: {\"id\":41,\"ts\":\"2026-08-30T12:00:00Z\",\"kind\":\"work.created\",\"entity_type\":\"work\",\"entity_id\":\"w1\",\"payload\":{}}\n" +
		"\n" +
		"event: other\n" +
		"data: {\"id\":42}\n" +
		"\n" +
		"event: journal\n" +
		"id: 43\n" +
		"data: {\"id\":43,\"ts\":\"2026-08-30T12:00:01Z\",\"kind\":\"target.transition\",\"entity_type\":\"target\",\"entity_id\":\"t1\",\"payload\":{\"to\":\"failed\"}}\n" +
		"\n"
	var got []journalEntry
	err := parseSSE(strings.NewReader(stream), func(e journalEntry) error {
		got = append(got, e)
		return nil
	})
	if err != nil {
		t.Fatalf("parseSSE: %v", err)
	}
	if len(got) != 2 {
		t.Fatalf("emitted %d entries, want 2 (the non-journal event is skipped)", len(got))
	}
	if got[0].ID != 41 || got[0].Kind != "work.created" || got[0].EntityID != "w1" {
		t.Errorf("first entry = %+v", got[0])
	}
	if got[1].ID != 43 || got[1].Kind != "target.transition" {
		t.Errorf("second entry = %+v", got[1])
	}
}

func TestParseSSEEmitErrorStops(t *testing.T) {
	stream := "event: journal\ndata: {\"id\":1}\n\nevent: journal\ndata: {\"id\":2}\n\n"
	boom := errors.New("stop")
	var n int
	err := parseSSE(strings.NewReader(stream), func(journalEntry) error {
		n++
		return boom
	})
	if !errors.Is(err, boom) {
		t.Fatalf("err = %v, want the emit error", err)
	}
	if n != 1 {
		t.Fatalf("emit ran %d times, want 1", n)
	}
}

func TestParseSSEBadJSON(t *testing.T) {
	stream := "event: journal\ndata: {not json\n\n"
	if err := parseSSE(strings.NewReader(stream), func(journalEntry) error { return nil }); err == nil {
		t.Fatal("want an error for an undecodable frame")
	}
}
