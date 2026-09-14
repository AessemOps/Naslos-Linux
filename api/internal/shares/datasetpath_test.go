package shares

import "testing"

// TestPathOnDataset is the guard that keeps share data on ZFS. The failure it
// prevents is silent: a path that is merely inside the ZFS base directory (a
// plain directory on the node's ephemeral partition) is served happily by
// Samba, but its data is not in the pool and is lost on a Talos upgrade.
func TestPathOnDataset(t *testing.T) {
	// The real layout that caused data loss: only /var/mnt/test is a dataset;
	// /var/mnt/tank and /var/mnt/Pog are plain directories.
	mounts := []string{"/", "none", "-", "/var/mnt/test"}

	cases := []struct {
		path      string
		wantOK    bool
		wantMatch string
	}{
		{"/var/mnt/test", true, "/var/mnt/test"},
		{"/var/mnt/test", true, "/var/mnt/test"},
		{"/var/mnt/test/media", true, "/var/mnt/test"}, // subdirectory of a dataset
		{"/var/mnt/tank", false, ""},                   // plain directory: must be refused
		{"/var/mnt/Pog", false, ""},                    // ditto
		{"/var/mnt", false, ""},                        // the base itself is not a dataset
		{"/etc", false, ""},
		{"relative/path", false, ""},
	}

	for _, tc := range cases {
		got, ok := PathOnDataset(tc.path, mounts)
		if ok != tc.wantOK || got != tc.wantMatch {
			t.Errorf("PathOnDataset(%q) = (%q, %v), want (%q, %v)",
				tc.path, got, ok, tc.wantMatch, tc.wantOK)
		}
	}
}

// TestPathOnDatasetPrefersMostSpecificMount covers nested datasets: the share's
// storage must be attributed to the innermost dataset, not its parent.
func TestPathOnDatasetPrefersMostSpecificMount(t *testing.T) {
	mounts := []string{"/var/mnt/test", "/var/mnt/test/media"}

	got, ok := PathOnDataset("/var/mnt/test/media/movies", mounts)
	if !ok || got != "/var/mnt/test/media" {
		t.Fatalf("PathOnDataset = (%q, %v), want /var/mnt/test/media", got, ok)
	}

	// A sibling of the nested dataset belongs to the parent.
	got, ok = PathOnDataset("/var/mnt/test/other", mounts)
	if !ok || got != "/var/mnt/test" {
		t.Fatalf("PathOnDataset = (%q, %v), want /var/mnt/test", got, ok)
	}
}

// TestPathOnDatasetIgnoresUnusableMountpoints makes sure a mountpoint that
// cannot hold user data never matches - "/" in particular would match
// everything and silently accept non-dataset paths.
func TestPathOnDatasetIgnoresUnusableMountpoints(t *testing.T) {
	for _, mp := range []string{"/", "none", "-", "", "  "} {
		if _, ok := PathOnDataset("/var/mnt/tank", []string{mp}); ok {
			t.Errorf("mountpoint %q must not validate a path", mp)
		}
	}
}
