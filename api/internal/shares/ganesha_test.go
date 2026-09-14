package shares

import (
	"strings"
	"testing"
)

// TestGenerateGaneshaConfig pins the NFS config that a userspace server
// consumes. Talos has no kernel nfsd, so this is the only thing an NFS client
// ever sees: getting the pseudo path or Access_Type wrong silently exports the
// wrong thing.
func TestGenerateGaneshaConfig(t *testing.T) {
	m := NewManagerWithBase("", t.TempDir())
	m.shares["media"] = &Share{
		Name: "media", Path: "/var/mnt/test/media", Protocol: ProtocolNFS,
		Enabled: true, AllowedHosts: []string{"192.168.1.0/24"},
	}
	m.shares["backups"] = &Share{
		Name: "backups", Path: "/var/mnt/test/backups", Protocol: ProtocolNFS,
		Enabled: true, ReadOnly: true,
	}
	m.shares["disabled"] = &Share{
		Name: "disabled", Path: "/var/mnt/test/disabled", Protocol: ProtocolNFS,
		Enabled: false,
	}
	m.shares["smbonly"] = &Share{
		Name: "smbonly", Path: "/var/mnt/test/smb", Protocol: ProtocolSMB, Enabled: true,
	}

	conf := m.GenerateGaneshaConfig()

	// NFSv4-only in userspace: no lock manager, no quota RPC, single port.
	for _, want := range []string{
		"Protocols = 4;",
		"Enable_NLM = false;",
		"NFS_Port = 2049;",
		"mount_path_pseudo = true;",
		"Name = VFS;",
	} {
		if !strings.Contains(conf, want) {
			t.Errorf("config missing %q:\n%s", want, conf)
		}
	}

	// Exported shares, mounted by pseudo path.
	if !strings.Contains(conf, "Path = /var/mnt/test/media;") {
		t.Errorf("read-write export missing:\n%s", conf)
	}
	if !strings.Contains(conf, "Pseudo = /media;") {
		t.Errorf("pseudo path missing (clients mount host:/media):\n%s", conf)
	}
	if !strings.Contains(conf, "Clients = 192.168.1.0/24;") {
		t.Errorf("client restriction missing:\n%s", conf)
	}
	if !strings.Contains(conf, "Path = /var/mnt/test/backups;") ||
		!strings.Contains(conf, "Access_Type = RO;") {
		t.Errorf("read-only export not marked RO:\n%s", conf)
	}

	// No host restriction means open to any client.
	if !strings.Contains(conf, "Clients = *;") {
		t.Errorf("unrestricted export should use Clients = *:\n%s", conf)
	}

	// A disabled NFS share and an SMB-only share must not be exported.
	if strings.Contains(conf, "/var/mnt/test/disabled") {
		t.Errorf("disabled share was exported:\n%s", conf)
	}
	if strings.Contains(conf, "/var/mnt/test/smb") {
		t.Errorf("SMB share was exported over NFS:\n%s", conf)
	}
}

// TestGaneshaExportIDsAreUniqueAndStable guards two requirements at once: every
// EXPORT needs a unique non-zero id, and the id must not change when unrelated
// shares are added or removed (clients hold references to exports).
func TestGaneshaExportIDsAreUniqueAndStable(t *testing.T) {
	names := []string{"media", "backups", "photos", "naslos_users"}
	seen := make(map[int]string, len(names))
	for _, n := range names {
		id := ganeshaExportID(n)
		if id <= 0 {
			t.Fatalf("export id for %q must be positive, got %d", n, id)
		}
		if other, dup := seen[id]; dup {
			t.Fatalf("export id collision between %q and %q: %d", n, other, id)
		}
		seen[id] = n

		if again := ganeshaExportID(n); again != id {
			t.Fatalf("export id for %q changed between calls: %d != %d", n, again, id)
		}
	}

	// Same id regardless of what other shares exist.
	first := ganeshaExportID("media")
	m1 := NewManagerWithBase("", t.TempDir())
	m1.shares["media"] = &Share{Name: "media", Path: "/var/mnt/test/media", Protocol: ProtocolNFS, Enabled: true}
	id1 := ganeshaExportID("media")
	if id1 != first {
		t.Fatalf("export id depends on manager state: %d != %d", id1, first)
	}
}

// TestGaneshaConfigWithNoSharesStillServes checks the empty case: a valid config
// with no EXPORT blocks, so ganesha.nfsd starts and simply exports nothing.
func TestGaneshaConfigWithNoSharesStillServes(t *testing.T) {
	m := NewManagerWithBase("", t.TempDir())
	conf := m.GenerateGaneshaConfig()
	if strings.Contains(conf, "EXPORT {") {
		t.Fatalf("no shares configured but an export was rendered:\n%s", conf)
	}
	if !strings.Contains(conf, "NFS_CORE_PARAM {") {
		t.Fatalf("core parameters missing:\n%s", conf)
	}
}
