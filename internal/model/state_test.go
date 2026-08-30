package model

import "testing"

func TestTransition(t *testing.T) {
	cases := []struct {
		from, to State
		ok       bool
	}{
		{Pending, Claimed, true},
		{Pending, Cancelled, true},
		{Pending, Running, false},
		{Pending, Succeeded, false},
		{Claimed, Preparing, true},
		{Claimed, Failed, true},
		{Claimed, Cancelled, true},
		{Claimed, Running, false},
		{Claimed, Pending, false},
		{Preparing, Running, true},
		{Preparing, Failed, true},
		{Preparing, Cancelled, true},
		{Preparing, Succeeded, false},
		{Running, Succeeded, true},
		{Running, Failed, true},
		{Running, Cancelled, true},
		{Running, Pending, false},
		{Running, Running, false},
		{Succeeded, Failed, false},
		{Failed, Pending, false},
		{Cancelled, Claimed, false},
		{State("bogus"), Claimed, false},
		{Pending, State("bogus"), false},
	}
	for _, c := range cases {
		err := Transition(c.from, c.to)
		if (err == nil) != c.ok {
			t.Errorf("Transition(%s, %s): ok=%v want %v (err=%v)", c.from, c.to, err == nil, c.ok, err)
		}
	}
}

func TestPredicates(t *testing.T) {
	for _, s := range []State{Succeeded, Failed, Cancelled} {
		if !Terminal(s) || Active(s) {
			t.Errorf("%s should be terminal and not active", s)
		}
	}
	for _, s := range []State{Claimed, Preparing, Running} {
		if Terminal(s) || !Active(s) {
			t.Errorf("%s should be active and not terminal", s)
		}
	}
	if Terminal(Pending) || Active(Pending) || !Valid(Pending) || Valid("nope") {
		t.Error("pending predicates wrong")
	}
}

func TestWorkState(t *testing.T) {
	cases := []struct {
		name    string
		targets []State
		want    State
	}{
		{"empty", nil, Failed},
		{"all pending", []State{Pending, Pending}, Pending},
		{"one claimed", []State{Pending, Claimed}, Running},
		{"running", []State{Running}, Running},
		{"terminal with pending", []State{Succeeded, Pending}, Running},
		{"all succeeded", []State{Succeeded, Succeeded}, Succeeded},
		{"all cancelled", []State{Cancelled}, Cancelled},
		{"all failed", []State{Failed, Failed}, Failed},
		{"failed and cancelled", []State{Failed, Cancelled}, WorkPartial},
		{"succeeded and failed", []State{Succeeded, Failed}, WorkPartial},
		{"succeeded and cancelled", []State{Succeeded, Cancelled}, WorkPartial},
	}
	for _, c := range cases {
		if got := WorkState(c.targets); got != c.want {
			t.Errorf("%s: got %s want %s", c.name, got, c.want)
		}
	}
}
