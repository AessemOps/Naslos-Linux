package shares

import (
	"fmt"
	"strings"
	"testing"
)

// TestRenderGroupIncludesLDAPGroups is what makes `valid users = @group` work:
// Samba resolves the group through NSS and checks the session user against its
// members. Without the LDAP group in this file the group is unknown on the node
// and every member is refused.
func TestRenderGroupIncludesLDAPGroups(t *testing.T) {
	store := NewSambaUserStore("")
	for _, u := range []PosixIdentity{
		{UID: "alice", UIDNum: 10001, GIDNum: 10000},
		{UID: "bob", UIDNum: 10002, GIDNum: 10000},
	} {
		if err := store.Upsert(u, "AABB"); err != nil {
			t.Fatalf("Upsert(%s): %v", u.UID, err)
		}
	}

	adminsGID := GroupGID("naslos_admins")
	out := store.RenderGroup([]NSSGroup{
		{Name: "naslos_admins", GID: adminsGID, Members: []string{"alice"}},
		{Name: "naslos_users", GID: GroupGID("naslos_users"), Members: []string{"alice", "bob"}},
	})

	if !strings.Contains(out, fmt.Sprintf("naslos_admins:x:%d:alice\n", adminsGID)) {
		t.Errorf("admins group missing or wrong:\n%s", out)
	}
	if !strings.Contains(out, "bob") {
		t.Errorf("group members missing:\n%s", out)
	}
}

// TestGroupGIDIsStable guards the requirement that a group keeps the same gid
// across renders: LDAP groups have no gidNumber, so an unstable derived value
// would make the group change identity between syncs and break access.
func TestGroupGIDIsStable(t *testing.T) {
	first := GroupGID("naslos_admins")
	for i := 0; i < 5; i++ {
		if got := GroupGID("naslos_admins"); got != first {
			t.Fatalf("gid changed between calls: %d != %d", got, first)
		}
	}
	if first == GroupGID("naslos_users") {
		t.Fatal("distinct group names produced the same gid")
	}
	// Must not collide with the primary gid range used by real users.
	if first < 20000 || first > 27999 {
		t.Fatalf("group gid %d is outside the reserved range", first)
	}
}

// TestAccessListRendersGroups covers the smb.conf side of group access.
func TestAccessListRendersGroups(t *testing.T) {
	cases := []struct {
		name    string
		share   Share
		want    string
		notWant string
	}{
		{
			name:  "groups are prefixed with @",
			share: Share{ValidGroups: []string{"naslos_users", "naslos_admins"}},
			want:  "valid users = @naslos_users @naslos_admins",
		},
		{
			name:  "users and groups combined",
			share: Share{ValidUsers: []string{"alice"}, ValidGroups: []string{"naslos_users"}},
			want:  "valid users = alice @naslos_users",
		},
		{
			name:  "an operator-typed @group is passed through",
			share: Share{ValidUsers: []string{"@naslos_admins"}},
			want:  "valid users = @naslos_admins",
		},
		{
			// A stray '@' would make Samba read the remainder as a group name.
			name:    "a stray @ is stripped from a user name",
			share:   Share{ValidUsers: []string{"ali@ce"}},
			want:    "valid users = alice",
			notWant: "@",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			m := NewManagerWithBase("", t.TempDir())
			m.shares["s"] = &Share{Name: "s", Path: "/var/mnt/tank", Protocol: ProtocolSMB, Enabled: true,
				ValidUsers: tc.share.ValidUsers, ValidGroups: tc.share.ValidGroups}
			conf := m.GenerateSambaConfig()
			if !strings.Contains(conf, tc.want) {
				t.Errorf("smb.conf missing %q:\n%s", tc.want, conf)
			}
			if tc.notWant != "" && strings.Contains(strings.SplitN(conf, "valid users", 2)[1], tc.notWant) {
				t.Errorf("smb.conf contains unwanted %q:\n%s", tc.notWant, conf)
			}
		})
	}
}

// TestUnrestrictedShareHasNoValidUsers checks the empty case stays absent, so a
// share with no access list remains open to any authenticated user.
func TestUnrestrictedShareHasNoValidUsers(t *testing.T) {
	m := NewManagerWithBase("", t.TempDir())
	m.shares["s"] = &Share{Name: "s", Path: "/var/mnt/tank", Protocol: ProtocolSMB, Enabled: true}
	if conf := m.GenerateSambaConfig(); strings.Contains(conf, "valid users") {
		t.Fatalf("expected no valid users line:\n%s", conf)
	}
}

// TestRenderPasswdSkipsAccountsWithoutUid guards the failure mode that made
