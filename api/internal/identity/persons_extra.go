package identity

import (
	"encoding/binary"
	"fmt"
	"strconv"
	"unicode/utf16"

	"github.com/go-ldap/ldap/v3"
	"golang.org/x/crypto/md4"
)

// UpdatePerson updates person attributes.
func (c *Client) UpdatePerson(uid, displayName, email, firstName, lastName string) error {
	uid = normalizeUID(uid)
	dn := fmt.Sprintf("uid=%s,ou=people,%s", uid, c.baseDN)

	modReq := ldap.NewModifyRequest(dn, nil)
	if displayName != "" {
		modReq.Replace("displayName", []string{displayName})
		modReq.Replace("cn", []string{displayName})
	}
	if email != "" {
		modReq.Replace("mail", []string{email})
	}
	if firstName != "" {
		modReq.Replace("givenName", []string{firstName})
	}
	if lastName != "" {
		modReq.Replace("sn", []string{lastName})
	}

	return c.do(func(conn *ldap.Conn) error {
		return conn.Modify(modReq)
	})
}

// DeletePerson removes a person.
func (c *Client) DeletePerson(uid string) error {
	uid = normalizeUID(uid)
	dn := fmt.Sprintf("uid=%s,ou=people,%s", uid, c.baseDN)

	delReq := ldap.NewDelRequest(dn, nil)
	return c.do(func(conn *ldap.Conn) error {
		return conn.Del(delReq)
	})
}

// SetPassword sets the password for a person and returns the NT hash for SMB sync.
func (c *Client) SetPassword(uid, password string) (string, error) {
	uid = normalizeUID(uid)
	dn := fmt.Sprintf("uid=%s,ou=people,%s", uid, c.baseDN)

	// Use LDAP Password Modify extended operation (RFC 3062)
	passwordModify := ldap.NewPasswordModifyRequest(dn, "", password)
	if err := c.do(func(conn *ldap.Conn) error {
		_, err := conn.PasswordModify(passwordModify)
		return err
	}); err != nil {
		return "", fmt.Errorf("setting password: %w", err)
	}

	// Compute NT hash for SMB sync
	ntHash := computeNTHash(password)
	return ntHash, nil
}

// GetUIDNumber returns the POSIX uidNumber of a person, used when mirroring
// the account into Samba (which keys its passdb on the POSIX uid).
func (c *Client) GetUIDNumber(uid string) (int, error) {
	uid = normalizeUID(uid)
	filter := fmt.Sprintf("(uid=%s)", uid)
	searchBase := fmt.Sprintf("ou=people,%s", c.baseDN)

	searchReq := ldap.NewSearchRequest(
		searchBase,
		ldap.ScopeWholeSubtree, ldap.NeverDerefAliases, 1, 0, false,
		filter,
		[]string{"uidNumber"},
		nil,
	)

	var result *ldap.SearchResult
	if err := c.do(func(conn *ldap.Conn) error {
		var err error
		result, err = conn.Search(searchReq)
		return err
	}); err != nil {
		return 0, fmt.Errorf("searching for uidNumber: %w", err)
	}

	if len(result.Entries) == 0 {
		return 0, fmt.Errorf("person %q not found", uid)
	}

	value := result.Entries[0].GetAttributeValue("uidNumber")
	if value == "" {
		return 0, fmt.Errorf("person %q has no uidNumber", uid)
	}

	n, err := strconv.Atoi(value)
	if err != nil {
		return 0, fmt.Errorf("person %q has an invalid uidNumber %q: %w", uid, value, err)
	}
	return n, nil
}

// EnablePerson enables a person account.
func (c *Client) EnablePerson(uid string) error {
	uid = normalizeUID(uid)
	dn := fmt.Sprintf("uid=%s,ou=people,%s", uid, c.baseDN)

	modReq := ldap.NewModifyRequest(dn, nil)
	modReq.Replace("shadowExpire", []string{"-1"})
	return c.do(func(conn *ldap.Conn) error {
		return conn.Modify(modReq)
	})
}

// DisablePerson disables a person account.
func (c *Client) DisablePerson(uid string) error {
	uid = normalizeUID(uid)
	dn := fmt.Sprintf("uid=%s,ou=people,%s", uid, c.baseDN)

	modReq := ldap.NewModifyRequest(dn, nil)
	modReq.Replace("shadowExpire", []string{"1"})
	return c.do(func(conn *ldap.Conn) error {
		return conn.Modify(modReq)
	})
}

// computeNTHash computes the NT hash (MD4 of UTF-16LE password) for SMB.
func computeNTHash(password string) string {
	h := md4.New()
	utf16Password := utf16.Encode([]rune(password))
	buf := make([]byte, 2*len(utf16Password))
	for i, r := range utf16Password {
		binary.LittleEndian.PutUint16(buf[i*2:], r)
	}
	h.Write(buf)
	return fmt.Sprintf("%X", h.Sum(nil))
}

// hashUID generates a simple hash for UID number generation.
func hashUID(uid string) int {
	h := 0
	for _, c := range uid {
		h = 31*h + int(c)
	}
	if h < 0 {
		h = -h
	}
	return h%9000 + 1000
}
