package server

import (
	"errors"
	"reflect"
	"testing"
)

// TestGroupDelta pins the membership diff used by PUT /api/users/{uid}: the
// current and desired lists become an explicit add/remove pair so an edited
// user's groups actually change (CR-18).
func TestGroupDelta(t *testing.T) {
	tests := []struct {
		name       string
		current    []string
		desired    []string
		wantAdd    []string
		wantRemove []string
	}{
		{name: "add only", current: []string{"users"}, desired: []string{"users", "admins"}, wantAdd: []string{"admins"}},
		{name: "remove only", current: []string{"users", "admins"}, desired: []string{"users"}, wantRemove: []string{"admins"}},
		{name: "swap", current: []string{"a"}, desired: []string{"b"}, wantAdd: []string{"b"}, wantRemove: []string{"a"}},
		{name: "no-op", current: []string{"a"}, desired: []string{"a"}},
		{name: "case insensitive", current: []string{"Admins"}, desired: []string{"admins"}},
		{name: "blank and duplicate entries", current: []string{"", "a", "a"}, desired: []string{"b", "b", ""}, wantAdd: []string{"b"}, wantRemove: []string{"a"}},
		{name: "remove all", current: []string{"a", "b"}, desired: []string{}, wantRemove: []string{"a", "b"}},
		{name: "add to empty membership", current: nil, desired: []string{"a"}, wantAdd: []string{"a"}},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			add, remove := groupDelta(tc.current, tc.desired)
			if !sameSet(add, tc.wantAdd) {
				t.Errorf("add = %v, want %v", add, tc.wantAdd)
			}
			if !sameSet(remove, tc.wantRemove) {
				t.Errorf("remove = %v, want %v", remove, tc.wantRemove)
			}
		})
	}
}

func sameSet(got, want []string) bool {
	if len(got) != len(want) {
		return false
	}
	if len(got) == 0 {
		return true
	}
	return reflect.DeepEqual(got, want)
}

// TestMembershipDeltaSharedNormalizer pins that the group-detail path reuses the
// same delta implementation with its own normalizer (CR-18 duplication fix):
// DNs and bare uids are compared as the same entry.
func TestMembershipDeltaSharedNormalizer(t *testing.T) {
	add, remove := membershipDelta(
		[]string{"uid=alice,ou=people,dc=naslos,dc=local"},
		[]string{"alice", "uid=bob,ou=people,dc=naslos,dc=local"},
		extractUID,
	)
	if !sameSet(add, []string{"bob"}) {
		t.Errorf("add = %v, want [bob]", add)
	}
	if len(remove) != 0 {
		t.Errorf("remove = %v, want none", remove)
	}
}

// TestApplyMembershipChangesAttemptsEveryChange pins the shared failure policy:
// one failed change must not strand the rest of the delta, and the first error
// is returned to the caller.
func TestApplyMembershipChangesAttemptsEveryChange(t *testing.T) {
	firstErr := errors.New("boom")
	var added, removed []string

	err := applyMembershipChanges(
		[]string{"a", "b"},
		[]string{"c", "d"},
		func(v string) error {
			added = append(added, v)
			if v == "a" {
				return firstErr
			}
			return nil
		},
		func(v string) error {
			removed = append(removed, v)
			return nil
		},
	)

	if !errors.Is(err, firstErr) {
		t.Errorf("err = %v, want the first failure", err)
	}
	if !reflect.DeepEqual(added, []string{"a", "b"}) {
		t.Errorf("added = %v, want every add attempted", added)
	}
	if !reflect.DeepEqual(removed, []string{"c", "d"}) {
		t.Errorf("removed = %v, want every remove attempted", removed)
	}
}
