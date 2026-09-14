package shares

import (
	"strings"
	"testing"
)

// TestNetBIOSNameSanitised pins the NetBIOS rules (uppercase, single label of
// at most 15 characters, Samba-safe characters only). The name must match what
// the serving container publishes over mDNS/WSD, and Samba rejects anything
// else at startup.
func TestNetBIOSNameSanitised(t *testing.T) {
	cases := []struct {
		in   string
		want string
	}{
		{"naslos", "NASLOS"},
		{"Naslos.local", "NASLOS"},
		{"my nas!los", "MYNASLOS"},
		{"abcdefghijklmnopqrstuvwxyz", "ABCDEFGHIJKLMNO"}, // truncated to 15
		{"", ""},
		{"!!!", ""},
	}
	for _, tc := range cases {
		if got := sanitizeNetBIOSName(tc.in); got != tc.want {
			t.Errorf("sanitizeNetBIOSName(%q) = %q, want %q", tc.in, got, tc.want)
		}
	}
}

// TestSambaConfAdvertisesNetBIOSName checks the rendered smb.conf carries the
// configured name, which is what makes the mDNS/WSD advertisement and the SMB
// server itself present one identity to clients.
func TestSambaConfAdvertisesNetBIOSName(t *testing.T) {
	t.Setenv("SMB_NETBIOS_NAME", "naslos")

	m := NewManagerWithBase("", t.TempDir())
	conf := m.GenerateSambaConfig()
	if !strings.Contains(conf, "netbios name = NASLOS") {
		t.Fatalf("netbios name missing from smb.conf:\n%s", conf)
	}

	// Unset: leave it to Samba rather than emitting a broken value.
	t.Setenv("SMB_NETBIOS_NAME", "")
	if conf := m.GenerateSambaConfig(); strings.Contains(conf, "netbios name") {
		t.Fatalf("no name configured but smb.conf declares one:\n%s", conf)
	}
}
