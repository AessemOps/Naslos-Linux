package identity

import (
	"strings"
	"testing"
)

// TestValidateIdentityNameRejectsMetacharacters pins the allowlist: these values
// reach search filters, DNs, file paths and Samba's passwd records, so anything
// that could change the meaning of a filter or a DN is refused up front (NAS-006).
func TestValidateIdentityNameRejectsMetacharacters(t *testing.T) {
	bad := []string{
		`*)(uid=*`,              // closes the filter and opens another condition
		`a,b`,                   // DN component separator
		`a b`,                   // space (Samba records, shell-adjacent)
		"a\nb",                  // newline (config injection)
		"a\rb",                  //
		"a\x00b",                // NUL
		`a\b`,                   // DN escape character
		`a+b`,                   // DN escape character
		"-flag",                 // looks like an option
		`../escape`,             // traversal
		"Admin",                 // uppercase: names are normalized, not guessed at
		"",                      // empty
		strings.Repeat("a", 65), // too long
	}
	for _, value := range bad {
		if err := validateIdentityName("username", value); err == nil {
			t.Errorf("validateIdentityName(%q) = nil, want a rejection", value)
		}
	}

	good := []string{"smbuser1", "naslos_admins", "naslos-service", "a.b-c", "u1", "n0de"}
	for _, value := range good {
		if err := validateIdentityName("username", value); err != nil {
			t.Errorf("validateIdentityName(%q) = %v, want it accepted", value, err)
		}
	}
}

// TestGroupNamesAreNormalizedAndValidated keeps group lookups and creations on one
// spelling, and refuses a cn that would break out of a DN or filter.
func TestGroupNamesAreNormalizedAndValidated(t *testing.T) {
	normalized, err := normalizeGroupName("Naslos_Users")
	if err != nil {
		t.Fatalf("normalizeGroupName(Naslos_Users) = %v, want it accepted", err)
	}
	if normalized != "naslos_users" {
		t.Errorf("normalized = %q, want naslos_users", normalized)
	}

	for _, value := range []string{"a,b", "x)(cn=*", "a\nb", "", "naslos admins"} {
		if _, err := normalizeGroupName(value); err == nil {
			t.Errorf("normalizeGroupName(%q) = nil error, want a rejection", value)
		}
	}
}

// TestSearchValuesAreEscaped checks the second line of defence: even if a value
// reached the LDAP layer, the filter escaping must neutralise the metacharacters
// rather than let them be interpreted.
func TestSearchValuesAreEscaped(t *testing.T) {
	escaped := escapeFilter("x)(uid=*")
	for _, meta := range []string{"(", ")", "*"} {
		if strings.Contains(escaped, meta) {
			t.Errorf("escapeFilter left %q in %q", meta, escaped)
		}
	}

	dn := escapeDNComponent("a,b")
	if !strings.Contains(dn, `\,`) {
		t.Errorf("escapeDNComponent(a,b) = %q, want the comma escaped", dn)
	}
}
