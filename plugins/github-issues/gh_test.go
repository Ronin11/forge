package main

import "testing"

func TestDecodeIssues(t *testing.T) {
	data := []byte(`[
		{"number": 12, "title": "crash on start", "body": "steps...", "url": "https://github.com/o/n/issues/12"},
		{"number": 13, "title": "typo", "body": "", "url": "https://github.com/o/n/issues/13"}
	]`)
	got, err := decodeIssues(data)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 2 {
		t.Fatalf("got %d issues, want 2", len(got))
	}
	if got[0].Number != 12 || got[0].Title != "crash on start" || got[0].URL == "" {
		t.Errorf("issue[0] = %+v", got[0])
	}
}

func TestDecodeIssuesSkipsMalformed(t *testing.T) {
	// A row with number 0 (or negative) could never become a task; it is
	// dropped rather than aborting the whole list.
	data := []byte(`[
		{"number": 0, "title": "no number", "url": "x"},
		{"number": 7, "title": "real", "url": "https://github.com/o/n/issues/7"}
	]`)
	got, err := decodeIssues(data)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || got[0].Number != 7 {
		t.Fatalf("malformed issue not skipped: %+v", got)
	}
}

func TestDecodeIssuesBadJSON(t *testing.T) {
	if _, err := decodeIssues([]byte(`{not an array`)); err == nil {
		t.Error("bad JSON accepted")
	}
}

func TestDecodeIssuesEmpty(t *testing.T) {
	got, err := decodeIssues([]byte(`[]`))
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 0 {
		t.Errorf("empty list decoded to %d", len(got))
	}
}
