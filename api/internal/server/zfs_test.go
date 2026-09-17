package server

import (
	"strings"
	"testing"
)

// The API checks the node's real inventory before forwarding a pool or vdev
// request, so an unknown, duplicated or already-attached disk is a fast 400
// instead of a rejected (or destructive) agent call. The agent validates too;
// this is the cheap half of that pair and must not drift.

// TestValidateAddDisksRefusals covers the checks that happen before the API talks
// to the node at all (so a Server with no talos/agent client is enough).
func TestValidateAddDisksRefusals(t *testing.T) {
	s := &Server{}

	cases := []struct {
		name     string
		topology string
		disks    []string
		want     string
	}{
		{"unknown topology", "raidz9", []string{"/dev/sdb"}, "unsupported topology"},
		{"no disks", "single", nil, "select at least one disk"},
		{"mirror with one disk", "mirror", []string{"/dev/sdb"}, "needs at least 2 disks"},
		{"raidz2 with two disks", "raidz2", []string{"/dev/sdb", "/dev/sdc"}, "needs at least 3 disks"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := s.validateAddDisks(tc.topology, tc.disks)
			if err == nil {
				t.Fatalf("validateAddDisks(%q, %v) = nil, want an error", tc.topology, tc.disks)
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error = %q, want it to mention %q", err, tc.want)
			}
		})
	}
}

// TestValidateDiskSelection covers the inventory-dependent rules against a fake
// inventory: the point is that a system disk, a foreign pool's disk, a duplicate
// or a non-/dev path never reaches the agent.
func TestValidateDiskSelection(t *testing.T) {
	// /dev/sda is the node's system disk (absent from usable), /dev/sdc belongs
	// to another pool, /dev/sdb is the only free and usable disk.
	usable := map[string]bool{"/dev/sdb": true, "/dev/sdc": true}
	inPool := map[string]string{"/dev/sdc": "tank"}

	if err := validateDiskSelection([]string{"/dev/sdb"}, usable, inPool); err != nil {
		t.Errorf("validateDiskSelection(free disk) = %v, want nil", err)
	}
	if err := validateDiskSelection([]string{"/dev/sdb", "/dev/sdc"}, map[string]bool{"/dev/sdb": true, "/dev/sdc": true}, nil); err != nil {
		t.Errorf("validateDiskSelection(two free disks) = %v, want nil", err)
	}

	cases := []struct {
		name  string
		disks []string
		want  string
	}{
		{"relative path", []string{"sdb"}, "absolute path under /dev"},
		{"not under /dev", []string{"/tmp/sdb"}, "absolute path under /dev"},
		{"unknown disk", []string{"/dev/sdz"}, "not a disk this node can use"},
		{"system disk", []string{"/dev/sda"}, "not a disk this node can use"},
		{"duplicate", []string{"/dev/sdb", "/dev/sdb"}, "selected twice"},
		{"already in a pool", []string{"/dev/sdc"}, "already belongs to pool"},
		{"empty string", []string{""}, "absolute path under /dev"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := validateDiskSelection(tc.disks, usable, inPool)
			if err == nil {
				t.Fatalf("validateDiskSelection(%v) = nil, want an error", tc.disks)
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error = %q, want it to mention %q", err, tc.want)
			}
		})
	}
}

// The pool-detail and import paths enforce the pool-name charset; a name like
// "-f" or "../etc" must be refused before it reaches the agent.
func TestPoolNamePatternGuard(t *testing.T) {
	for _, bad := range []string{"-f", "../etc", "tank/name", "tank pool", ""} {
		if poolNamePattern.MatchString(bad) {
			t.Errorf("poolNamePattern accepted %q", bad)
		}
	}
	for _, good := range []string{"tank", "pool1", "my-pool", "a.b_c:d"} {
		if !poolNamePattern.MatchString(good) {
			t.Errorf("poolNamePattern rejected %q", good)
		}
	}
}
