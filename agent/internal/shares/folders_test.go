package shares

import (
	"context"
	"os"
	"path/filepath"
	"testing"
)

// TestCleanFolderPath covers the guard that keeps folder operations inside the
// datasets. The API is reachable from the UI, so a traversal here would be a
// remote file-creation primitive on the node.
func TestCleanFolderPath(t *testing.T) {
	cases := []struct {
		in      string
		want    string
		wantErr bool
	}{
		{"/var/mnt/test", "/var/mnt/test", false},
		{"/var/mnt/test/media", "/var/mnt/test/media", false},
		{"/var/mnt/test/media/", "/var/mnt/test/media", false},
		{"/var/mnt/test/./media", "/var/mnt/test/media", false},
		{"  /var/mnt/test  ", "/var/mnt/test", false},

		{"", "", true},
		{"/var/mnt", "/var/mnt", false}, // the root itself may be listed
		{"/var/mnt/test/../../etc", "/var/etc", true},
		{"/var/mnt/test/../..", "/", true},
		{"/etc", "", true},
		{"/var/lib/naslos/shares", "", true},
		{"relative/path", "", true},
		{"/var/mnt-other/test", "", true}, // prefix must be a whole component
	}

	for _, tc := range cases {
		got, err := CleanFolderPath(tc.in)
		if tc.wantErr {
			if err == nil {
				t.Errorf("CleanFolderPath(%q) = %q, want an error", tc.in, got)
			}
			continue
		}
		if err != nil {
			t.Errorf("CleanFolderPath(%q) returned %v, want %q", tc.in, err, tc.want)
			continue
		}
		if got != tc.want {
			t.Errorf("CleanFolderPath(%q) = %q, want %q", tc.in, got, tc.want)
		}
	}
}

// TestValidateFolderName rejects names that would escape the folder, break the
// config files, or collide with ZFS internals.
func TestValidateFolderName(t *testing.T) {
	valid := []string{"media", "My Photos", "backup-2026", "a", "_hidden", "dossier"}
	for _, name := range valid {
		if err := ValidateFolderName(name); err != nil {
			t.Errorf("ValidateFolderName(%q) = %v, want nil", name, err)
		}
	}

	invalid := []string{
		"",
		"   ",
		" leading",
		"trailing ",
		".",
		"..",
		"a/b",
		"a\\b",
		"a\x00b",
		"two\nlines",
		".zfs", // ZFS's snapshot directory
		string(make([]byte, 256)),
	}
	for _, name := range invalid {
		if err := ValidateFolderName(name); err == nil {
			t.Errorf("ValidateFolderName(%q) = nil, want an error", name)
		}
	}
}

// TestFolderOperations drives list/create/delete against a temporary tree
// standing in for the host root, so the behaviour the API's folder endpoints
// depend on is covered without a node.
func TestFolderOperations(t *testing.T) {
	root := t.TempDir()
	previous := hostRoot
	hostRoot = root
	t.Cleanup(func() { hostRoot = previous })

	dataset := filepath.Join(root, "var/mnt/test")
	if err := os.MkdirAll(dataset, 0755); err != nil {
		t.Fatalf("setting up dataset dir: %v", err)
	}
	// ZFS's snapshot directory must never be offered as a share folder.
	if err := os.MkdirAll(filepath.Join(dataset, zfsSnapshotsDir), 0755); err != nil {
		t.Fatalf("setting up .zfs: %v", err)
	}

	c := NewClient(context.Background())

	folders, err := c.ListFolders("/var/mnt/test")
	if err != nil {
		t.Fatalf("ListFolders: %v", err)
	}
	if len(folders) != 0 {
		t.Errorf("ListFolders = %v, want no folders (.zfs hidden)", folders)
	}

	created, err := c.CreateFolder("/var/mnt/test", "media")
	if err != nil {
		t.Fatalf("CreateFolder: %v", err)
	}
	if created != "/var/mnt/test/media" {
		t.Errorf("CreateFolder returned %q, want /var/mnt/test/media", created)
	}
	if info, err := os.Stat(filepath.Join(dataset, "media")); err != nil || !info.IsDir() {
		t.Errorf("folder was not created on disk: %v", err)
	}

	folders, err = c.ListFolders("/var/mnt/test")
	if err != nil {
		t.Fatalf("ListFolders after create: %v", err)
	}
	if len(folders) != 1 || folders[0] != "media" {
		t.Errorf("ListFolders = %v, want [media]", folders)
	}

	// A second create with the same name must be reported, not silently ignored.
	if _, err := c.CreateFolder("/var/mnt/test", "media"); err == nil {
		t.Error("CreateFolder over an existing folder returned nil, want an error")
	}

	// The parent has to exist: creating "media/nested" would otherwise put the
	// share in a folder the operator never created.
	if _, err := c.CreateFolder("/var/mnt/test/missing", "nested"); err == nil {
		t.Error("CreateFolder under a missing parent returned nil, want an error")
	}

	// An empty folder can be removed...
	if err := c.DeleteFolder("/var/mnt/test/media"); err != nil {
		t.Errorf("DeleteFolder(empty) = %v, want nil", err)
	}
	if _, err := os.Stat(filepath.Join(dataset, "media")); !os.IsNotExist(err) {
		t.Error("folder still present after DeleteFolder")
	}

	// ...but a non-empty one must not be, and neither must the dataset root:
	// there is no recycle bin, so a folder picker must not delete data.
	if err := os.MkdirAll(filepath.Join(dataset, "keep"), 0755); err != nil {
		t.Fatalf("setting up keep dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dataset, "keep", "file.txt"), []byte("data"), 0644); err != nil {
		t.Fatalf("writing file: %v", err)
	}
	if err := c.DeleteFolder("/var/mnt/test/keep"); err == nil {
		t.Error("DeleteFolder(non-empty) returned nil, want an error")
	}
	if err := c.DeleteFolder("/var/mnt"); err == nil {
		t.Error("DeleteFolder(datasets root) returned nil, want an error")
	}
}
