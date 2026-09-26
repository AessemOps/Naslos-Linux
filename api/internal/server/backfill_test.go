package server

import "testing"

func TestSplitChartLabel(t *testing.T) {
	cases := []struct {
		label   string
		name    string
		version string
	}{
		{"naslos-0.1.0", "naslos", "0.1.0"},
		{"hello-0.0.1", "hello", "0.0.1"},
		{"my-app-1.2.3", "my-app", "1.2.3"},
		{"no-suffix", "no-suffix", ""},
		{"trailing-", "trailing-", ""},
	}
	for _, tc := range cases {
		name, version := splitChartLabel(tc.label)
		if name != tc.name || version != tc.version {
			t.Errorf("splitChartLabel(%q) = (%q, %q), want (%q, %q)", tc.label, name, version, tc.name, tc.version)
		}
	}
}
