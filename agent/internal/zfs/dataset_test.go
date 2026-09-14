package zfs

import (
	"context"
	"strings"
	"testing"
)

// TestCreateDatasetCommandLine pins the `zfs create` invocation. Options must be
// sorted (map iteration is random) so the same request always produces the same
// command, and -p is required so a nested name creates its parents.
func TestCreateDatasetCommandLine(t *testing.T) {
	calls := captureRun(t)
	c := NewClient(context.Background())

	if err := c.CreateDataset("tank/media", map[string]string{
		"quota": "100G", "compression": "zstd",
	}); err != nil {
		t.Fatalf("CreateDataset: %v", err)
	}

	got := (*calls)[0]
	want := "/usr/local/sbin/zfs create -p -o compression=zstd -o quota=100G tank/media"
	if got != want {
		t.Errorf("command = %q, want %q", got, want)
	}
}

// TestCreateDatasetWithoutOptions keeps a plain create simple.
func TestCreateDatasetWithoutOptions(t *testing.T) {
	calls := captureRun(t)
	c := NewClient(context.Background())

	if err := c.CreateDataset("tank/photos/2026", nil); err != nil {
		t.Fatalf("CreateDataset: %v", err)
	}
	if got, want := (*calls)[0], "/usr/local/sbin/zfs create -p tank/photos/2026"; got != want {
		t.Errorf("command = %q, want %q", got, want)
	}
}

// TestCreateDatasetRefusals covers names and options that must never reach ZFS:
// `zfs create -o` accepts arbitrary properties, so an unchecked key/value could
// put the dataset somewhere unexpected.
func TestCreateDatasetRefusals(t *testing.T) {
	nameCases := []struct{ name, wantErr string }{
		{"tank", "must be <pool>/<name>"},
		{"tank/../etc", "invalid dataset name"},
		{"/tank/media", "not an absolute path"},
		{"tank/@snap", "must not contain '@'"},
		{"tank/media@snap", "must not contain '@'"},
		{"tank/a//b", "invalid dataset name"},
		{"tank/-leading", "invalid dataset name"},
		{"tank/sha res", "invalid dataset name"},
		{"", "required"},
	}
	for _, tc := range nameCases {
		t.Run(tc.name, func(t *testing.T) {
			calls := captureRun(t)
			c := NewClient(context.Background())

			err := c.CreateDataset(tc.name, nil)
			if err == nil || !strings.Contains(err.Error(), tc.wantErr) {
				t.Fatalf("CreateDataset(%q) = %v, want an error containing %q", tc.name, err, tc.wantErr)
			}
			if len(*calls) != 0 {
				t.Errorf("refused request still ran %v", *calls)
			}
		})
	}

	optionCases := []struct {
		name    string
		options map[string]string
		wantErr string
	}{
		{"unknown key", map[string]string{"shareiscsi": "on"}, "unsupported dataset option"},
		{"mountpoint is not settable here", map[string]string{"mountpoint": "/"}, "unsupported dataset option"},
		{"bad compression", map[string]string{"compression": "fastest"}, "unsupported compression"},
		{"bad quota", map[string]string{"quota": "lots"}, "invalid quota"},
		{"bad recordsize", map[string]string{"recordsize": "big"}, "invalid recordsize"},
		{"bad copies", map[string]string{"copies": "9"}, "copies must be 1, 2 or 3"},
		{"bad atime", map[string]string{"atime": "maybe"}, "must be on or off"},
	}
	for _, tc := range optionCases {
		t.Run(tc.name, func(t *testing.T) {
			calls := captureRun(t)
			c := NewClient(context.Background())

			err := c.CreateDataset("tank/media", tc.options)
			if err == nil || !strings.Contains(err.Error(), tc.wantErr) {
				t.Fatalf("CreateDataset(options=%v) = %v, want an error containing %q", tc.options, err, tc.wantErr)
			}
			if len(*calls) != 0 {
				t.Errorf("refused request still ran %v", *calls)
			}
		})
	}
}

// TestDestroyDatasetRequiresRecursiveFlag: without -r ZFS refuses a dataset that
// has children or snapshots, which is the safe default for a UI action.
func TestDestroyDatasetRequiresRecursiveFlag(t *testing.T) {
	calls := captureRun(t)
	c := NewClient(context.Background())

	if err := c.DestroyDataset("tank/media", false); err != nil {
		t.Fatalf("DestroyDataset: %v", err)
	}
	if got, want := (*calls)[0], "/usr/local/sbin/zfs destroy tank/media"; got != want {
		t.Errorf("command = %q, want %q", got, want)
	}

	calls = captureRun(t)
	if err := c.DestroyDataset("tank/media", true); err != nil {
		t.Fatalf("DestroyDataset(recursive): %v", err)
	}
	if got, want := (*calls)[0], "/usr/local/sbin/zfs destroy -r tank/media"; got != want {
		t.Errorf("command = %q, want %q", got, want)
	}

	// A pool cannot be destroyed through the dataset endpoint.
	calls = captureRun(t)
	if err := c.DestroyDataset("tank", true); err == nil {
		t.Error("DestroyDataset(pool) = nil, want an error")
	}
	if len(*calls) != 0 {
		t.Errorf("refused request still ran %v", *calls)
	}
}

// TestValidateDatasetName covers valid shapes, including nested names.
func TestValidateDatasetName(t *testing.T) {
	for _, ok := range []string{"media", "photos/2026", "a.b_c-d", "Media1", "a/b/c/d"} {
		if err := ValidateDatasetName(ok); err != nil {
			t.Errorf("ValidateDatasetName(%q) = %v, want nil", ok, err)
		}
	}
	for _, bad := range []string{"", " media", "media ", "../x", "a/../b", "a b", "-1-", "@", "a@b", "/abs"} {
		if err := ValidateDatasetName(bad); err == nil {
			t.Errorf("ValidateDatasetName(%q) = nil, want an error", bad)
		}
	}
}
