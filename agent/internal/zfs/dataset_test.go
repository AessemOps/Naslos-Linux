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

// TestDestroyDatasetRecoversFromStaleMount covers AV-8b: a dataset that was once
// mounted inside a pod's mount namespace keeps a stale in-kernel mount record, so
// `zfs destroy` reports "dataset is busy" forever even though nothing is mounted,
// the dataset has no snapshots or children, and a reboot does not clear it. The
// destroy must force an unmount and retry rather than leaving the dataset
// undestroyable.
func TestDestroyDatasetRecoversFromStaleMount(t *testing.T) {
	calls := captureRun(t,
		"ERR:cannot destroy 'tank/stale': dataset is busy\n",
		"", // zfs unmount -f
		"", // zfs destroy (retry)
	)
	c := NewClient(context.Background())

	if err := c.DestroyDataset("tank/stale", true); err != nil {
		t.Fatalf("DestroyDataset should recover from a stale mount, got: %v", err)
	}
	got := *calls
	if len(got) != 3 {
		t.Fatalf("expected destroy, unmount, destroy; got %v", got)
	}
	if want := "/usr/local/sbin/zfs destroy -r tank/stale"; got[0] != want {
		t.Errorf("first command = %q, want %q", got[0], want)
	}
	if want := "/usr/local/sbin/zfs unmount -f tank/stale"; got[1] != want {
		t.Errorf("recovery command = %q, want %q", got[1], want)
	}
	if got[2] != got[0] {
		t.Errorf("retry command = %q, want the original %q", got[2], got[0])
	}
}

// TestDestroyDatasetDoesNotUnmountOnOtherErrors: only the stale-mount case earns
// an unmount. A genuine refusal (in use, permissions) must surface as-is, or an
// unrelated error would silently unmount a live dataset.
func TestDestroyDatasetDoesNotUnmountOnOtherErrors(t *testing.T) {
	calls := captureRun(t, "ERR:cannot destroy 'tank/live': dataset is in use\n")
	c := NewClient(context.Background())

	if err := c.DestroyDataset("tank/live", false); err == nil {
		t.Fatal("DestroyDataset = nil, want the original error")
	}
	if len(*calls) != 1 {
		t.Errorf("a non-busy error must not trigger an unmount, ran %v", *calls)
	}
}

// TestDestroyDatasetReportsUnmountFailure keeps the failure legible: if the
// forced unmount fails for a reason OTHER than "not currently mounted", the
// error must say so rather than looking like the original busy refusal - a
// genuinely in-use dataset must not have its mountpoint cleared.
func TestDestroyDatasetReportsUnmountFailure(t *testing.T) {
	calls := captureRun(t,
		"ERR:dataset is busy\n",
		"ERR:dataset is in use\n",
	)
	c := NewClient(context.Background())

	err := c.DestroyDataset("tank/stale", false)
	if err == nil {
		t.Fatal("DestroyDataset = nil, want an error")
	}
	if !strings.Contains(err.Error(), "forced unmount") {
		t.Errorf("error = %v, want it to mention the forced unmount", err)
	}
	if len(*calls) != 2 {
		t.Errorf("a non-'not mounted' unmount failure must not clear the mountpoint, ran %v", *calls)
	}
}

// TestDestroyDatasetClearsStaleMountpoint covers the second rung of the AV-8b
// ladder: when `zfs unmount -f` reports "not currently mounted", the dataset is
// busy with no live mount at all (a dead namespace still references the path),
// and the documented fix is to clear the recorded mountpoint and retry. This is
// the state the live test/audit-av8 dataset was stuck in.
func TestDestroyDatasetClearsStaleMountpoint(t *testing.T) {
	calls := captureRun(t,
		"ERR:cannot destroy 'tank/stale': dataset is busy\n",
		"ERR:cannot unmount 'tank/stale': not currently mounted\n",
		"", // zfs set mountpoint=none
		"", // zfs destroy (retry)
	)
	c := NewClient(context.Background())

	if err := c.DestroyDataset("tank/stale", true); err != nil {
		t.Fatalf("DestroyDataset should clear the stale mountpoint and succeed, got: %v", err)
	}
	got := *calls
	if len(got) != 4 {
		t.Fatalf("expected destroy, unmount, set mountpoint=none, destroy; got %v", got)
	}
	if want := "/usr/local/sbin/zfs unmount -f tank/stale"; got[1] != want {
		t.Errorf("unmount command = %q, want %q", got[1], want)
	}
	if want := "/usr/local/sbin/zfs set mountpoint=none canmount=off tank/stale"; got[2] != want {
		t.Errorf("mountpoint command = %q, want %q", got[2], want)
	}
	if want := "/usr/local/sbin/zfs destroy -r tank/stale"; got[3] != want {
		t.Errorf("final destroy = %q, want %q", got[3], want)
	}
}

// TestHumanBytes covers the display formatter that replaced the raw `zfs list`
// column once -p was added for exact bytes (AV-8a).
func TestHumanBytes(t *testing.T) {
	cases := []struct {
		n    int64
		want string
	}{
		{0, "0B"},
		{512, "512B"},
		{1024, "1.0K"},
		{1536, "1.5K"},
		{10 * 1024, "10K"},
		{2 * 1024 * 1024 * 1024, "2.0G"},
	}
	for _, tc := range cases {
		if got := humanBytes(tc.n); got != tc.want {
			t.Errorf("humanBytes(%d) = %q, want %q", tc.n, got, tc.want)
		}
	}
}
