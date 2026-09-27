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
	m.shares["open"] = &Share{
		Name: "open", Path: "/var/mnt/test/open", Protocol: ProtocolNFS,
		Enabled: true, AllowedHosts: []string{"*"},
	}
	m.shares["unsquashed"] = &Share{
		Name: "unsquashed", Path: "/var/mnt/test/unsquashed", Protocol: ProtocolNFS,
		Enabled: true, AllowedHosts: []string{"192.168.1.0/24"}, NoRootSquash: true,
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

	// PF-H4: a share that names no hosts defaults to localhost only (with no
	// NASLOS_LAN_CIDR set here), never "*".
	if !strings.Contains(conf, "Clients = 127.0.0.1/32;") {
		t.Errorf("unrestricted export should fail closed to localhost:\n%s", conf)
	}
	// Explicit "*" is the only opt-in to an open export.
	if !strings.Contains(conf, "Clients = *;") {
		t.Errorf("an explicit '*' allowedHosts entry should export to any client:\n%s", conf)
	}
	// Root_Squash is the default; a share can opt out.
	if !strings.Contains(conf, "Squash = Root_Squash;") {
		t.Errorf("default squash should be Root_Squash:\n%s", conf)
	}
	if !strings.Contains(conf, "Squash = No_Root_Squash;") {
		t.Errorf("the per-share opt-out should render No_Root_Squash:\n%s", conf)
	}

	// A disabled NFS share and an SMB-only share must not be exported.
	if strings.Contains(conf, "/var/mnt/test/disabled") {
		t.Errorf("disabled share was exported:\n%s", conf)
	}
	if strings.Contains(conf, "/var/mnt/test/smb") {
		t.Errorf("SMB share was exported over NFS:\n%s", conf)
	}
}

// TestSambaConfigHardenedDefaults pins the PF-H4 SMB posture: no root-forcing,
// a fail-closed client default, tighter masks, and the global hardening lines.
func TestSambaConfigHardenedDefaults(t *testing.T) {
	t.Setenv("NASLOS_LAN_CIDR", "192.168.1.0/24")
	m := NewManagerWithBase("", t.TempDir())
	m.shares["media"] = &Share{
		Name: "media", Path: "/var/mnt/test/media", Protocol: ProtocolSMB, Enabled: true,
	}
	m.shares["guests"] = &Share{
		Name: "guests", Path: "/var/mnt/test/guests", Protocol: ProtocolSMB, Enabled: true,
		AllowedHosts: []string{"*"},
	}

	conf := m.GenerateSambaConfig()

	for _, want := range []string{
		"map to guest = Never",
		"server min protocol = SMB3",
		"smb encrypt = desired",
		"create mask = 0660",
		"directory mask = 0770",
		// A share with no hosts is limited to the configured LAN.
		"hosts allow = 192.168.1.0/24",
		"hosts deny = all",
	} {
		if !strings.Contains(conf, want) {
			t.Errorf("smb.conf missing %q:\n%s", want, conf)
		}
	}
	if strings.Contains(conf, "force user") || strings.Contains(conf, "force group") {
		t.Errorf("smb.conf still forces root:\n%s", conf)
	}
	// The explicit-wildcard share must not carry a hosts allow/deny pair. Slice
	// out just the [guests] section (up to the next section header) so a share
	// emitted after it cannot be mistaken for part of it.
	idx := strings.Index(conf, "[guests]")
	if idx < 0 {
		t.Fatalf("smb.conf has no [guests] block:\n%s", conf)
	}
	guests := conf[idx:]
	if end := strings.Index(guests, "\n["); end >= 0 {
		guests = guests[:end]
	}
	if strings.Contains(guests, "hosts allow") {
		t.Errorf("explicit '*' should not be restricted by hosts allow:\n%s", conf)
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
