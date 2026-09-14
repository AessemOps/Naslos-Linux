package shares

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// TestRenderSMBPasswdFormat pins the smbpasswd line format that
// `pdbedit -i smbpasswd:<file>` consumes, including the disabled flag.
func TestRenderSMBPasswdFormat(t *testing.T) {
	store := NewSambaUserStore("")

	if err := store.Upsert(PosixIdentity{UID: "jdoe", UIDNum: 10123, GIDNum: 10000}, "6574cc330574c3fb138e544592d125c9"); err != nil {
		t.Fatalf("Upsert: %v", err)
	}
	// Hashes are stored uppercase regardless of input casing.
	if got, _ := store.Get("jdoe"); got.NTHash != "6574CC330574C3FB138E544592D125C9" {
		t.Fatalf("hash not normalised to uppercase: %q", got.NTHash)
	}

	out := store.RenderSMBPasswd()
	line := ""
	for _, l := range strings.Split(out, "\n") {
		if strings.HasPrefix(l, "jdoe:") {
			line = l
			break
		}
	}
	if line == "" {
		t.Fatalf("no smbpasswd line rendered:\n%s", out)
	}

	fields := strings.Split(line, ":")
	if len(fields) != 7 {
		t.Fatalf("expected 7 colon-separated fields, got %d: %q", len(fields), line)
	}
	if fields[0] != "jdoe" {
		t.Errorf("username field = %q, want jdoe", fields[0])
	}
	if fields[1] != "10123" {
		t.Errorf("uid field = %q, want 10123 (LDAP uidNumber)", fields[1])
	}
	if fields[2] != noLMHash {
		t.Errorf("LM hash field = %q, want no-hash placeholder", fields[2])
	}
	if fields[3] != "6574CC330574C3FB138E544592D125C9" {
		t.Errorf("NT hash field = %q", fields[3])
	}
	if fields[4] != "[U          ]" {
		t.Errorf("flags field = %q, want enabled flags", fields[4])
	}
	if !strings.HasPrefix(fields[5], "LCT-") {
		t.Errorf("LCT field = %q, want LCT-<hex>", fields[5])
	}
}

// TestDisabledAccountKeepsHash checks that disabling an account preserves its
// NT hash (so re-enabling needs no new password) and sets the disabled flag.
func TestDisabledAccountKeepsHash(t *testing.T) {
	store := NewSambaUserStore("")
	if err := store.Upsert(PosixIdentity{UID: "jdoe", UIDNum: 10123, GIDNum: 10000}, "AABB"); err != nil {
		t.Fatalf("Upsert: %v", err)
	}
	if err := store.SetEnabled("jdoe", false); err != nil {
		t.Fatalf("SetEnabled: %v", err)
	}

	out := store.RenderSMBPasswd()
	if !strings.Contains(out, "[DU         ]") {
		t.Fatalf("disabled flag missing:\n%s", out)
	}
	if !strings.Contains(out, "AABB") {
		t.Fatalf("hash dropped when disabling:\n%s", out)
	}
}

// TestStorePersistsAcrossReload verifies the mirror survives an API restart,
// which is what keeps SMB logins working after a pod restart.
func TestStorePersistsAcrossReload(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "smbusers.json")

	store := NewSambaUserStore(path)
	if err := store.Upsert(PosixIdentity{UID: "jdoe", UIDNum: 10123, GIDNum: 10000}, "AABBCC"); err != nil {
		t.Fatalf("Upsert: %v", err)
	}
	if _, err := os.Stat(path); err != nil {
		t.Fatalf("store not written: %v", err)
	}

	reloaded := NewSambaUserStore(path)
	got, ok := reloaded.Get("jdoe")
	if !ok {
		t.Fatal("account lost after reload")
	}
	if got.NTHash != "AABBCC" || got.UIDNumber != 10123 {
		t.Fatalf("account not restored: %+v", got)
	}
}

// TestRenderNSSFiles checks the extrausers files that let the serving
// container resolve LDAP users to UNIX uids. Without these Samba cannot attach
// a session to a uid and every login is denied.
func TestRenderNSSFiles(t *testing.T) {
	store := NewSambaUserStore("")
	if err := store.Upsert(PosixIdentity{
		UID: "smbtest", UIDNum: 19330, GIDNum: 10000, Gecos: "Smb Test",
	}, "6574CC330574C3FB138E544592D125C9"); err != nil {
		t.Fatalf("Upsert: %v", err)
	}

	passwd := store.RenderPasswd()
	if !strings.Contains(passwd, "smbtest:x:19330:10000:Smb Test:/home/smbtest:/bin/bash") {
		t.Errorf("passwd entry wrong:\n%s", passwd)
	}

	group := store.RenderGroup(nil)
	if !strings.Contains(group, "naslos_users:x:10000:smbtest") {
		t.Errorf("primary group entry wrong:\n%s", group)
	}

	shadow := store.RenderShadow()
	if !strings.Contains(shadow, "smbtest:*:") {
		t.Errorf("shadow entry wrong:\n%s", shadow)
	}
}

// TestRenderPasswdSkipsAccountsWithoutUid guards the failure mode that made
// SMB logins fail silently: an account with no POSIX uid must not be rendered,
// because Samba would store uid 4294967295 and deny the login.
func TestRenderPasswdSkipsAccountsWithoutUid(t *testing.T) {
	store := NewSambaUserStore("")
	if err := store.Upsert(PosixIdentity{UID: "nouid"}, "AABB"); err != nil {
		t.Fatalf("Upsert: %v", err)
	}
	if out := store.RenderPasswd(); strings.Contains(out, "nouid") {
		t.Fatalf("account without a uid must not be rendered:\n%s", out)
	}
	// It is still imported (the NT hash is valid), so the operator can see the
	// account and the entrypoint's NSS check can warn about it.
	if out := store.RenderSMBPasswd(); !strings.Contains(out, "nouid") {
		t.Fatalf("account should still be rendered for the passdb:\n%s", out)
	}
}
func TestRemoveDeletesAccount(t *testing.T) {
	store := NewSambaUserStore("")
	if err := store.Upsert(PosixIdentity{UID: "jdoe", UIDNum: 1, GIDNum: 10000}, "AABB"); err != nil {
		t.Fatalf("Upsert: %v", err)
	}
	if err := store.Remove("jdoe"); err != nil {
		t.Fatalf("Remove: %v", err)
	}
	if _, ok := store.Get("jdoe"); ok {
		t.Fatal("account still present after Remove")
	}
	if strings.Contains(store.RenderSMBPasswd(), "jdoe") {
		t.Fatal("removed account still rendered")
	}
}
