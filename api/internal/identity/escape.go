package identity

import (
	"fmt"
	"regexp"
	"strings"

	"github.com/go-ldap/ldap/v3"
)

// identityNamePattern is the allowlist for a uid or a group cn: the appliance's
// own convention (`naslos_admins`, `naslos_users`, `smbuser1`). Inputs outside it
// are refused rather than escaped and hoped for, because these values also reach
// file paths and Samba's passwd/smbpasswd records (NAS-006).
var identityNamePattern = regexp.MustCompile(`^[a-z0-9][a-z0-9._-]{0,63}$`)

// validateIdentityName rejects a uid/cn outside the allowlist.
func validateIdentityName(kind, value string) error {
	if !identityNamePattern.MatchString(value) {
		return fmt.Errorf("invalid %s %q: use lowercase letters, digits, '.', '_' or '-', "+
			"starting with a letter or digit, at most 64 characters", kind, value)
	}
	return nil
}

// escapeDNComponent escapes a value used as one DN component value (RFC 4514).
// The allowlist above already excludes the metacharacters, so this is defense in
// depth: a future caller that skips validation still cannot break out of the DN.
func escapeDNComponent(value string) string {
	return ldap.EscapeDN(value)
}

// escapeFilter escapes a value interpolated into a search filter (RFC 4515).
// Without it `*`, `(`, `)` and NUL change the filter's meaning entirely.
func escapeFilter(value string) string {
	return ldap.EscapeFilter(value)
}

// normalizeGroupName lowercases and validates a group cn, the same way uids are
// normalized, so lookups and creations agree on one spelling.
func normalizeGroupName(cn string) (string, error) {
	normalized := strings.ToLower(strings.TrimSpace(cn))
	if err := validateIdentityName("group name", normalized); err != nil {
		return "", err
	}
	return normalized, nil
}

// NormalizeGroupName exposes the canonical group-name rule so callers that
// compare or accept group names use the same spelling and allowlist the
// AddMember/RemoveMember path enforces, instead of re-deriving one.
func NormalizeGroupName(cn string) (string, error) {
	return normalizeGroupName(cn)
}
