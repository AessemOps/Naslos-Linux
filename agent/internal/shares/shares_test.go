package shares

import (
	"context"
	"os"
	"testing"
)

// TestApplyRestrictsSecretMirrors is the CR-15 regression test: the files that
// carry hashes (the Samba passdb mirror and the NSS shadow mirror) must not be
// world-readable, while the non-secret NSS passwd/group mirrors stay 0644.
func TestApplyRestrictsSecretMirrors(t *testing.T) {
	prev := hostRoot
	hostRoot = t.TempDir()
	defer func() { hostRoot = prev }()

	c := NewClient(context.Background())
	cfg := Config{
		SambaConf:   "[global]\n   workgroup = NASLOS\n",
		GaneshaConf: "EXPORT {\n}\n",
		SambaUsers:  "alice:0:NO_PASSWORD:8846F7EAEE8FB117AD06BDD830B7586C:[U          ]:LCT-00000000:\n",
		NSSPasswd:   "alice:x:10001:10000::/home/alice:/bin/bash\n",
		NSSGroup:    "users:x:10000:\n",
		NSSShadow:   "alice:$6$hash:19000:0:99999:7:::\n",
		Revision:    "7",
	}
	if _, err := c.Apply(cfg); err != nil {
		t.Fatalf("Apply: %v", err)
	}

	for _, tc := range []struct {
		path string
		mode os.FileMode
	}{
		{SMBUsersPath, 0600},
		{NSSDir + "/shadow", 0600},
		{NSSDir + "/passwd", 0644},
		{NSSDir + "/group", 0644},
	} {
		info, err := os.Stat(hostPath(tc.path))
		if err != nil {
			t.Fatalf("stat %s: %v", tc.path, err)
		}
		if got := info.Mode().Perm(); got != tc.mode {
			t.Errorf("%s mode = %04o, want %04o", tc.path, got, tc.mode)
		}
	}

	// An upgrade rewrites nothing when the rendered content is unchanged, but a
	// mode change (the shadow mirror's 0644 -> 0600) must still be applied.
	if err := os.Chmod(hostPath(NSSDir+"/shadow"), 0644); err != nil {
		t.Fatalf("simulating the pre-upgrade mode: %v", err)
	}
	if _, err := c.Apply(cfg); err != nil {
		t.Fatalf("second Apply: %v", err)
	}
	info, err := os.Stat(hostPath(NSSDir + "/shadow"))
	if err != nil {
		t.Fatalf("stat shadow after the second Apply: %v", err)
	}
	if got := info.Mode().Perm(); got != 0600 {
		t.Errorf("shadow mode after a content-identical Apply = %04o, want 0600", got)
	}
}
