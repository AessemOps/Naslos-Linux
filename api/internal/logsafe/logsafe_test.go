package logsafe

import (
	"strings"
	"testing"
)

// TestFieldStripsControlCharacters is the AUDIT-L5 regression: a name carrying a
// newline must not be able to forge a second log entry.
func TestFieldStripsControlCharacters(t *testing.T) {
	cases := map[string]string{
		"plain":            "plain",
		"tank/ds":          "tank/ds",
		"evil\ninjected":   "evil.injected",
		"carriage\rreturn": "carriage.return",
		"tab\there":        "tab.here",
		"ansi\x1b[31mred":  "ansi.[31mred",
		"del\x7fchar":      "del.char",
		"unicode-é-ok":     "unicode-é-ok",
		"":                 "",
	}
	for in, want := range cases {
		if got := Field(in); got != want {
			t.Errorf("Field(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestFieldCapsLength(t *testing.T) {
	long := strings.Repeat("a", 500)
	got := Field(long)
	if len(got) > maxFieldLen+3 {
		t.Errorf("Field capped to %d bytes, want <= %d", len(got), maxFieldLen+3)
	}
	if !strings.HasSuffix(got, "...") {
		t.Errorf("Field(%d chars) = %q..., want a trailing ellipsis", len(long), got[:10])
	}
}
