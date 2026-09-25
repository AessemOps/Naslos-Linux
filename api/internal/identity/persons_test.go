package identity

import "testing"

// TestPersonCN covers the fix for creating a user without displayName: LDAP
// rejects a zero-length cn with "value #0 invalid per syntax", and displayName
// is optional in the API, so it must be derived from the name parts or the uid.
func TestPersonCN(t *testing.T) {
	cases := []struct {
		display, first, last, uid, want string
	}{
		{"Explicit Name", "A", "B", "u1", "Explicit Name"},
		{"", "Admin", "User", "u2", "Admin User"},
		{"", "Admin", "", "u3", "Admin"},
		{"", "", "User", "u4", "User"},
		{"", "", "", "u5", "u5"},
		{"   ", "  ", "  ", "u6", "u6"},
	}
	for _, tc := range cases {
		if got := personCN(tc.display, tc.first, tc.last, tc.uid); got != tc.want {
			t.Errorf("personCN(%q,%q,%q,%q) = %q, want %q", tc.display, tc.first, tc.last, tc.uid, got, tc.want)
		}
		if got := personCN(tc.display, tc.first, tc.last, tc.uid); got == "" {
			t.Errorf("personCN must never return an empty cn")
		}
	}
}
