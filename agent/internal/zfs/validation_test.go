package zfs

import (
	"context"
	"errors"
	"strings"
	"testing"
)

// The tests in this file pin the agent-side input validation (NAS-003): a
// privileged, host-networked agent must never let caller input reach
// `zpool`/`zfs`/`wipefs` argv unchecked. Every refusal asserts that no command
// ran at all, not merely that an error came back.

// ranCommand reports whether the recorded calls contain the given substring.
func ranCommand(calls []string, substr string) bool {
	for _, call := range calls {
		if strings.Contains(call, substr) {
			return true
		}
	}
	return false
}

// The HTTP layer answers 400 for these: a caller-fixable input must be
// distinguishable from a node failure (NAS-003), so the validators classify
// themselves as ValidationError rather than a bare error.
func TestValidatorsClassifyAsValidationErrors(t *testing.T) {
	cases := []struct {
		name string
		err  error
	}{
		{"pool name", ValidatePoolName("-f")},
		{"dataset name", ValidateDatasetName("../etc")},
		{"dataset path", validateDatasetPath("data")},
		{"snapshot name", ValidateSnapshotName("a/b")},
		{"dataset options", ValidateDatasetOptions(map[string]string{"mountpoint": "/"})},
		{"topology", func() error { _, err := NormalizeVDevTopology("raidz9"); return err }()},
		{"disk path", func() error { _, err := normalizeDiskPath("sdb"); return err }()},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if tc.err == nil {
				t.Fatal("validator accepted invalid input")
			}
			var invalid *ValidationError
			if !errors.As(tc.err, &invalid) {
				t.Errorf("error %v is not a *ValidationError, so the HTTP layer would answer 500", tc.err)
			}
		})
	}
}

func TestCreatePoolRefusalsBeforeAnyHostCall(t *testing.T) {
	cases := []struct {
		name string
		cfg  PoolConfig
		want string
	}{
		{"flag-shaped pool name", PoolConfig{Name: "-f", Disks: []string{"/dev/sdb"}}, "pool name"},
		{"path-shaped pool name", PoolConfig{Name: "../etc", Disks: []string{"/dev/sdb"}}, "pool name"},
		{"unknown topology", PoolConfig{Name: "tank", Topology: "raidz9", Disks: []string{"/dev/sdb"}}, "topology"},
		{"no disks", PoolConfig{Name: "tank"}, "at least one disk"},
		{"mirror with one disk", PoolConfig{Name: "tank", Topology: "mirror", Disks: []string{"/dev/sdb"}}, "needs at least 2"},
		{"unsupported option", PoolConfig{Name: "tank", Disks: []string{"/dev/sdb"}, Options: map[string]string{"mountpoint": "/"}}, "unsupported dataset option"},
		{"bad compression", PoolConfig{Name: "tank", Disks: []string{"/dev/sdb"}, Options: map[string]string{"compression": "magic"}}, "unsupported compression"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			calls := captureRun(t)
			c := NewClient(context.Background())

			err := c.CreatePool(tc.cfg)
			if err == nil {
				t.Fatalf("CreatePool(%+v) = nil, want an error", tc.cfg)
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error = %q, want it to mention %q", err, tc.want)
			}
			if ranCommand(*calls, "create") || ranCommand(*calls, "wipefs") {
				t.Errorf("refused request still ran %v", *calls)
			}
		})
	}
}

func TestCreatePoolRefusesBadDisks(t *testing.T) {
	cases := []struct {
		name string
		cfg  PoolConfig
		want string
	}{
		{"relative disk", PoolConfig{Name: "tank", Disks: []string{"sdb"}}, "absolute path"},
		{"traversal in path", PoolConfig{Name: "tank", Disks: []string{"/dev/../etc/passwd"}}, "invalid disk path"},
		{"absent device", PoolConfig{Name: "tank", Disks: []string{"/dev/sdz"}}, "not found"},
		{"duplicate disk", PoolConfig{Name: "tank", Disks: []string{"/dev/sdb", "/dev/sdb"}}, "more than once"},
		{"bad cache device", PoolConfig{Name: "tank", Disks: []string{"/dev/sdb"}, Cache: "/dev/sdz"}, "not found"},
		{"cache is also a data disk", PoolConfig{Name: "tank", Disks: []string{"/dev/sdb"}, Cache: "/dev/sdb"}, "both a data disk and the cache"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			// 1: the pool does not exist yet; 2: no pools, so no disk is a member.
			calls := captureRun(t, "ERR:cannot open 'tank': no such pool\n", "")
			c := NewClient(context.Background())

			err := c.CreatePool(tc.cfg)
			if err == nil {
				t.Fatalf("CreatePool(%+v) = nil, want an error", tc.cfg)
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error = %q, want it to mention %q", err, tc.want)
			}
			if ranCommand(*calls, " create ") {
				t.Errorf("refused request still ran %v", *calls)
			}
		})
	}
}

// A disk that belongs to another pool must never be handed to `zpool create -f`,
// which would overwrite the label and destroy that pool.
func TestCreatePoolRefusesDiskInAnotherPool(t *testing.T) {
	calls := captureRun(t,
		"ERR:cannot open 'tank': no such pool\n",
		"other\t10G\t1G\t9G\tONLINE\n",
		"config:\n\n"+
			"        NAME   STATE  READ WRITE CKSUM\n"+
			"        other  ONLINE 0 0 0\n"+
			"          /dev/sdb ONLINE 0 0 0\n\n",
	)
	c := NewClient(context.Background())

	err := c.CreatePool(PoolConfig{Name: "tank", Disks: []string{"/dev/sdb"}})
	if err == nil || !strings.Contains(err.Error(), "already belongs to pool") {
		t.Fatalf("error = %v, want it to name the owning pool", err)
	}
	if ranCommand(*calls, " create ") {
		t.Errorf("refused request still ran %v", *calls)
	}
}

// TestCreatePoolCommandLine pins the accepted invocation, including the
// normalized disk paths and the cache add.
func TestCreatePoolCommandLine(t *testing.T) {
	calls := captureRun(t, "ERR:cannot open 'tank': no such pool\n", "")
	c := NewClient(context.Background())

	err := c.CreatePool(PoolConfig{
		Name:     "tank",
		Topology: "mirror",
		Disks:    []string{"/dev/sdb", "/dev/sdc"},
		Cache:    "/dev/sdd",
		Options:  map[string]string{"compression": "lz4"},
	})
	if err != nil {
		t.Fatalf("CreatePool = %v, want nil", err)
	}

	want := "zpool create -f -o ashift=12 tank mirror /dev/sdb /dev/sdc"
	if !ranCommand(*calls, want) {
		t.Errorf("calls = %v, want %q", *calls, want)
	}
	if !ranCommand(*calls, "zpool add tank cache /dev/sdd") {
		t.Errorf("calls = %v, want the cache device added", *calls)
	}
}

// Every pool-name sink is validated before it reaches argv. A name like "-f" or
// a path would otherwise be read by zpool as a flag or a different target.
func TestPoolSinksRefuseInvalidNames(t *testing.T) {
	bad := []string{"", "  ", "-f", "tank/name", "../etc", "tank pool"}

	for _, name := range bad {
		calls := captureRun(t)
		c := NewClient(context.Background())

		if err := c.DestroyPool(name); err == nil {
			t.Errorf("DestroyPool(%q) = nil, want an error", name)
		}
		if _, err := c.PoolStatus(name); err == nil {
			t.Errorf("PoolStatus(%q) = nil error, want an error", name)
		}
		if _, err := c.PoolHealth(name); err == nil {
			t.Errorf("PoolHealth(%q) = nil error, want an error", name)
		}
		if err := c.ExportPool(name); err == nil {
			t.Errorf("ExportPool(%q) = nil, want an error", name)
		}
		if _, err := c.Datasets(name); err == nil {
			t.Errorf("Datasets(%q) = nil error, want an error", name)
		}

		if len(*calls) != 0 {
			t.Errorf("refused names still ran %v", *calls)
		}
	}
}

// ImportPool("") is the deliberate "import every pool" form used at agent
// startup, so only a non-empty name is validated.
func TestImportPoolValidatesANamedPool(t *testing.T) {
	calls := captureRun(t)
	c := NewClient(context.Background())

	if err := c.ImportPool("-f"); err == nil {
		t.Error("ImportPool(\"-f\") = nil, want an error")
	}
	if err := c.ImportPool("tank/name"); err == nil {
		t.Error("ImportPool(\"tank/name\") = nil, want an error")
	}
	if len(*calls) != 0 {
		t.Errorf("refused names still ran %v", *calls)
	}

	calls = captureRun(t)
	if err := c.ImportPool(""); err != nil {
		t.Errorf("ImportPool(\"\") = %v, want the import-all form to be accepted", err)
	}
	if !ranCommand(*calls, "zpool import -f") {
		t.Errorf("calls = %v, want the import-all invocation", *calls)
	}
}

// Snapshot takes a dataset *and* a snapshot name; both reach `zfs snapshot`.
func TestSnapshotRefusesInvalidArguments(t *testing.T) {
	cases := []struct {
		name    string
		dataset string
		snap    string
		want    string
	}{
		{"flag-shaped snapshot", "tank/data", "-r", "must not start with '-'"},
		{"snapshot with a slash", "tank/data", "a/b", "must not contain"},
		{"snapshot with a space", "tank/data", "a b", "must not contain"},
		{"dataset without a pool", "data", "snap1", "must be <pool>/<name>"},
		{"absolute dataset", "/tank/data", "snap1", "not an absolute path"},
		{"dataset with a snapshot", "tank/data@old", "snap1", "must not contain '@'"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			calls := captureRun(t)
			c := NewClient(context.Background())

			err := c.Snapshot(tc.dataset, tc.snap)
			if err == nil {
				t.Fatalf("Snapshot(%q, %q) = nil, want an error", tc.dataset, tc.snap)
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error = %q, want it to mention %q", err, tc.want)
			}
			if len(*calls) != 0 {
				t.Errorf("refused request still ran %v", *calls)
			}
		})
	}

	// The well-formed case still goes through.
	calls := captureRun(t)
	c := NewClient(context.Background())
	if err := c.Snapshot("tank/data", "buddy-20260101T000000Z-abcd"); err != nil {
		t.Fatalf("Snapshot(valid) = %v, want nil", err)
	}
	if !ranCommand(*calls, "zfs snapshot tank/data@buddy-20260101T000000Z-abcd") {
		t.Errorf("calls = %v, want the snapshot to be created", *calls)
	}
}

// Snapshots(dataset) is used by the backup sender, so a bad path must not reach
// `zfs list -r`.
func TestSnapshotsRefusesInvalidDataset(t *testing.T) {
	calls := captureRun(t)
	c := NewClient(context.Background())

	if _, err := c.Snapshots("tank/data@old"); err == nil {
		t.Error("Snapshots(dataset with '@') = nil error, want an error")
	}
	if len(*calls) != 0 {
		t.Errorf("refused request still ran %v", *calls)
	}
}
