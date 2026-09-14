package zfs

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// captureRun replaces the host runner for the duration of a test, recording the
// command line instead of executing it. These operations are destructive and
// irreversible, so asserting the arguments is the point of the test.
func captureRun(t *testing.T, responses ...string) *[]string {
	t.Helper()

	defaultRun := runHost
	t.Cleanup(func() { runHost = defaultRun })

	var calls []string
	idx := 0

	// Fake host root with the devices the tests refer to, so path validation
	// passes for real-looking disks and fails for absent ones.
	root := t.TempDir()
	if err := os.MkdirAll(filepath.Join(root, "dev"), 0755); err != nil {
		t.Fatalf("setting up fake /dev: %v", err)
	}
	for _, name := range []string{"sdb", "sdc", "sdd", "sde"} {
		if err := os.WriteFile(filepath.Join(root, "dev", name), nil, 0600); err != nil {
			t.Fatalf("creating fake device: %v", err)
		}
	}
	prevRoot := hostRoot
	hostRoot = root
	t.Cleanup(func() { hostRoot = prevRoot })

	runHost = func(ctx context.Context, name string, args ...string) (string, error) {
		calls = append(calls, strings.Join(append([]string{name}, args...), " "))
		if idx < len(responses) {
			out := responses[idx]
			idx++
			if strings.HasPrefix(out, "ERR:") {
				return strings.TrimPrefix(out, "ERR:"), os.ErrInvalid
			}
			return out, nil
		}
		return "", nil
	}
	return &calls
}

// TestAddVDevCommandLine pins the exact `zpool add` invocation for each topology:
// a wrong argument here attaches the wrong device to a live pool.
func TestAddVDevCommandLine(t *testing.T) {
	cases := []struct {
		name     string
		topology string
		disks    []string
		force    bool
		want     string
	}{
		{
			name: "single disk", topology: "single", disks: []string{"/dev/sdb"},
			want: "/usr/local/sbin/zpool add tank /dev/sdb",
		},
		{
			name: "empty topology means stripe", topology: "", disks: []string{"/dev/sdb"},
			want: "/usr/local/sbin/zpool add tank /dev/sdb",
		},
		{
			name: "stripe spelling", topology: "stripe", disks: []string{"/dev/sdb", "/dev/sdc"},
			want: "/usr/local/sbin/zpool add tank /dev/sdb /dev/sdc",
		},
		{
			name: "mirror", topology: "mirror", disks: []string{"/dev/sdb", "/dev/sdc"},
			want: "/usr/local/sbin/zpool add tank mirror /dev/sdb /dev/sdc",
		},
		{
			name: "raidz2", topology: "raidz2", disks: []string{"/dev/sdb", "/dev/sdc", "/dev/sdd"},
			want: "/usr/local/sbin/zpool add tank raidz2 /dev/sdb /dev/sdc /dev/sdd",
		},
		{
			name: "force is opt-in and comes before the pool", topology: "mirror",
			disks: []string{"/dev/sdb", "/dev/sdc"}, force: true,
			want: "/usr/local/sbin/zpool add -f tank mirror /dev/sdb /dev/sdc",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			// Responses: `zpool list tank` (exists), `zpool list` (Pools), `zpool status`.
			calls := captureRun(t, "tank\n", "tank\t1G\n", "")
			c := NewClient(context.Background())

			if err := c.AddVDev("tank", tc.topology, tc.disks, tc.force); err != nil {
				t.Fatalf("AddVDev: %v", err)
			}

			all := *calls
			if len(all) == 0 {
				t.Fatal("no command was run")
			}
			if got := all[len(all)-1]; got != tc.want {
				t.Errorf("last command = %q, want %q\nall: %v", got, tc.want, all)
			}
		})
	}
}

// TestAddVDevRefusals covers what must never reach `zpool add`.
func TestAddVDevRefusals(t *testing.T) {
	cases := []struct {
		name     string
		pool     string
		topology string
		disks    []string
		wantErr  string
	}{
		{"no disks", "tank", "mirror", nil, "at least one disk"},
		{"mirror needs two", "tank", "mirror", []string{"/dev/sdb"}, "at least 2 disks"},
		{"raidz2 needs three", "tank", "raidz2", []string{"/dev/sdb", "/dev/sdc"}, "at least 3 disks"},
		{"raidz3 needs four", "tank", "raidz3", []string{"/dev/sdb", "/dev/sdc", "/dev/sdd"}, "at least 4 disks"},
		{"unknown topology", "tank", "raidz9", []string{"/dev/sdb"}, "unsupported topology"},
		{"bad pool name", "tank/../etc", "single", []string{"/dev/sdb"}, "invalid pool name"},
		{"relative disk", "tank", "single", []string{"sdb"}, "absolute path under /dev"},
		{"device does not exist", "tank", "single", []string{"/dev/sdq"}, "not found on the node"},
		{"duplicate disk", "tank", "single", []string{"/dev/sdb", "/dev/sdb"}, "more than once"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			calls := captureRun(t, "tank\n", "tank\t1G\n", "")
			c := NewClient(context.Background())

			err := c.AddVDev(tc.pool, tc.topology, tc.disks, false)
			if err == nil {
				t.Fatalf("AddVDev(%v) = nil, want an error containing %q", tc.disks, tc.wantErr)
			}
			if !strings.Contains(err.Error(), tc.wantErr) {
				t.Errorf("error = %q, want it to contain %q", err, tc.wantErr)
			}

			// Nothing destructive may have run.
			for _, call := range *calls {
				if strings.Contains(call, " add ") {
					t.Errorf("a refused request still ran %q", call)
				}
			}
		})
	}
}

// TestAddVDevRefusesDiskInAnotherPool is the guard against destroying a pool:
// `zpool add -f` on a member disk overwrites that pool's label.
func TestAddVDevRefusesDiskInAnotherPool(t *testing.T) {
	// Responses: `zpool list tank` (exists), `zpool list` (Pools, with "other"),
	// then `zpool status other` listing /dev/sdb as a member.
	calls := captureRun(t,
		"tank\n",
		"other\t10G\t1G\t9G\tONLINE\n",
		"config:\n\n"+
			"        NAME   STATE  READ WRITE CKSUM\n"+
			"        other  ONLINE 0 0 0\n"+
			"          /dev/sdb ONLINE 0 0 0\n\n",
	)
	c := NewClient(context.Background())

	err := c.AddVDev("tank", "single", []string{"/dev/sdb"}, true)
	if err == nil {
		t.Fatal("adding a disk that belongs to another pool returned nil")
	}
	if !strings.Contains(err.Error(), "already belongs to pool") {
		t.Errorf("error = %q, want it to name the owning pool", err)
	}
	for _, call := range *calls {
		if strings.Contains(call, " add ") {
			t.Errorf("refused request still ran %q", call)
		}
	}
}

// TestAddVDevMissingPoolIsReported makes sure a typo is a clear error rather than
// a ZFS message about a missing pool after the disks were prepared.
func TestAddVDevMissingPoolIsReported(t *testing.T) {
	calls := captureRun(t, "ERR:cannot open 'nosuch': no such pool\n")
	c := NewClient(context.Background())

	err := c.AddVDev("nosuch", "single", []string{"/dev/sdb"}, false)
	if err == nil || !strings.Contains(err.Error(), "not found") {
		t.Fatalf("error = %v, want a pool-not-found error", err)
	}
	for _, call := range *calls {
		if strings.Contains(call, " add ") {
			t.Errorf("refused request still ran %q", call)
		}
	}
}

// TestIsWholeDisk keeps partitions and unrelated devices out of the disk picker.
func TestIsWholeDisk(t *testing.T) {
	whole := []string{"sda", "vdb", "hdc", "nvme0n1"}
	notWhole := []string{"sda1", "vdb12", "nvme0n1p1", "loop0", "sr0", "zram0", "dm-0"}

	for _, name := range whole {
		if !isWholeDisk(name) {
			t.Errorf("isWholeDisk(%q) = false, want true", name)
		}
	}
	for _, name := range notWhole {
		if isWholeDisk(name) {
			t.Errorf("isWholeDisk(%q) = true, want false", name)
		}
	}
}

// TestValidatePoolName covers the input check the API relies on.
func TestValidatePoolName(t *testing.T) {
	for _, ok := range []string{"tank", "pool1", "my-pool", "a.b_c:d"} {
		if err := ValidatePoolName(ok); err != nil {
			t.Errorf("ValidatePoolName(%q) = %v, want nil", ok, err)
		}
	}
	for _, bad := range []string{"", "  ", "-tank", "tank/name", "tank pool", "../etc", "tank@snap"} {
		if err := ValidatePoolName(bad); err == nil {
			t.Errorf("ValidatePoolName(%q) = nil, want an error", bad)
		}
	}
}
