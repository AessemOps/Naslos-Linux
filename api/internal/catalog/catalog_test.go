package catalog

import (
	"os"
	"path/filepath"
	"testing"
)

// TestGetReturnsADeepCopy is the PF-M7 regression: merging install-time values
// into DefaultValues must not mutate the shared catalog entry, or one install
// leaks its values into every later read (cross-admin credential disclosure).
func TestGetReturnsADeepCopy(t *testing.T) {
	dir := t.TempDir()
	app := `{
	  "name": "demo",
	  "displayName": "Demo",
	  "defaultValues": {
	    "adminPassword": "changeme",
	    "nested": {"token": "s3cret"}
	  },
	  "tags": ["a"]
	}`
	if err := os.WriteFile(filepath.Join(dir, "demo.json"), []byte(app), 0600); err != nil {
		t.Fatalf("writing app: %v", err)
	}

	c := New(dir)

	first, err := c.Get("demo")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	// A caller merges request values into the returned map and touches a nested
	// map, exactly as handleApps does.
	first.DefaultValues["adminPassword"] = "attacker-chosen"
	first.DefaultValues["nested"].(map[string]interface{})["token"] = "rewritten"
	first.Tags[0] = "mutated"

	second, err := c.Get("demo")
	if err != nil {
		t.Fatalf("second Get: %v", err)
	}
	if second.DefaultValues["adminPassword"] != "changeme" {
		t.Errorf("top-level default leaked out of Get: %v", second.DefaultValues["adminPassword"])
	}
	if nested := second.DefaultValues["nested"].(map[string]interface{}); nested["token"] != "s3cret" {
		t.Errorf("nested default leaked out of Get: %v", nested["token"])
	}
	if second.Tags[0] != "a" {
		t.Errorf("tags leaked out of Get: %v", second.Tags)
	}
}
