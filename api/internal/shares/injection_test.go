package shares

import (
	"path/filepath"
	"strings"
	"testing"
)

// controlPayloads are the shapes that would turn a share field into
// configuration: a newline starts a new smb.conf directive and a tab or NUL ends
// the line just as effectively (NAS-007).
var controlPayloads = []string{
	"x\nvalid users = alice",
	"x\r\nread only = no",
	"x\x00y",
	"x\ty",
}

// namePayloads add the punctuation that is dangerous specifically in a section
// header, which only the share *name* can become.
var namePayloads = append(append([]string{}, controlPayloads...), "[evil]", "a;b", "a=b")

func TestValidateShareFieldsRejectsNewDirectives(t *testing.T) {
	for _, payload := range controlPayloads {
		if err := validateShareFields(&Share{Description: payload}); err == nil {
			t.Errorf("description %q was accepted", payload)
		}
		if err := validateShareFields(&Share{Path: payload}); err == nil {
			t.Errorf("path %q was accepted", payload)
		}
		if err := validateShareFields(&Share{AllowedHosts: []string{payload}}); err == nil {
			t.Errorf("allowed host %q was accepted", payload)
		}
		if err := validateShareFields(&Share{ValidUsers: []string{payload}}); err == nil {
			t.Errorf("valid user %q was accepted", payload)
		}
		if err := validateShareFields(&Share{ValidGroups: []string{payload}}); err == nil {
			t.Errorf("valid group %q was accepted", payload)
		}
	}

	// Ganesha statement terminators and quotes are refused in list entries, where
	// they are interpolated into a `CLIENT { ... }` block.
	for _, entry := range []string{"alice;", `bob"`, "x;y"} {
		if err := validateShareFields(&Share{ValidUsers: []string{entry}}); err == nil {
			t.Errorf("valid user %q was accepted", entry)
		}
	}

	// Ordinary values still pass.
	ok := &Share{
		Name:         "media",
		Path:         "/var/mnt/tank/media",
		Description:  "Films and music",
		AllowedHosts: []string{"192.168.1.0/24", "naslos.lan"},
		ValidUsers:   []string{"alice", "@naslos_users"},
		ValidGroups:  []string{"naslos_users"},
	}
	if err := validateShareFields(ok); err != nil {
		t.Errorf("validateShareFields(valid share) = %v, want nil", err)
	}
}

// TestShareNameRejectsControlCharacters covers the name validator directly: it
// already blocked the punctuation that breaks a section header, and now blocks the
// characters that would end the line too.
func TestShareNameRejectsControlCharacters(t *testing.T) {
	for _, name := range namePayloads {
		if err := validateShareName(name); err == nil {
			t.Errorf("validateShareName(%q) = nil, want a rejection", name)
		}
	}
	if err := validateShareName("media"); err != nil {
		t.Errorf("validateShareName(media) = %v, want nil", err)
	}
}

// TestCreateRefusesInjectedFields goes through the manager: a request that would
// otherwise be stored and rendered must fail.
func TestCreateRefusesInjectedFields(t *testing.T) {
	m := NewManager(filepath.Join(t.TempDir(), "shares.json"))

	_, err := m.Create(CreateShareRequest{
		Name:        "media",
		Path:        "/var/mnt/tank/media",
		Protocol:    ProtocolSMB,
		Description: "films\nvalid users = alice",
	})
	if err == nil {
		t.Fatal("Create accepted a description containing a newline")
	}
	if !strings.Contains(err.Error(), "control characters") {
		t.Errorf("error = %q, want it to explain the control characters", err)
	}

	if _, err := m.Get("media"); err == nil {
		t.Error("the refused share was stored anyway")
	}
}

func TestNormalizeNTHash(t *testing.T) {
	got, err := normalizeNTHash("6574cc330574c3fb138e544592d125c9")
	if err != nil {
		t.Fatalf("normalizeNTHash(valid) = %v", err)
	}
	if got != "6574CC330574C3FB138E544592D125C9" {
		t.Errorf("normalizeNTHash = %q, want it upper-cased", got)
	}

	for _, bad := range []string{"", "AABB", "6574cc330574c3fb138e544592d125c9ff", "zzzzcc330574c3fb138e544592d125c9", "AABBCCDDEEFF0011223344556677\n899"} {
		if _, err := normalizeNTHash(bad); err == nil {
			t.Errorf("normalizeNTHash(%q) = nil error, want a rejection", bad)
		}
	}
}

func TestUpsertRejectsForgedRecords(t *testing.T) {
	store := NewSambaUserStore(filepath.Join(t.TempDir(), "smbusers.json"))

	// A ':' in the uid forges a passwd field; a newline forges a whole line.
	if err := store.Upsert(PosixIdentity{UID: "a:b", UIDNum: 1001, GIDNum: 1000}, "AABBCCDDEEFF00112233445566778899"); err == nil {
		t.Error("Upsert accepted a uid containing ':'")
	}
	if err := store.Upsert(PosixIdentity{UID: "ok", Gecos: "x\ny", UIDNum: 1001, GIDNum: 1000}, "AABBCCDDEEFF00112233445566778899"); err == nil {
		t.Error("Upsert accepted a gecos containing a newline")
	}
	if err := store.Upsert(PosixIdentity{UID: "ok", UIDNum: 1001, GIDNum: 1000}, "not-a-hash"); err == nil {
		t.Error("Upsert accepted a malformed NT hash")
	}

	if err := store.Upsert(PosixIdentity{UID: "ok", Gecos: "Ordinary Person", UIDNum: 1001, GIDNum: 1000}, "aabbccddeeff00112233445566778899"); err != nil {
		t.Errorf("Upsert(valid) = %v, want nil", err)
	}
}
