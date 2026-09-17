package identity

import (
	"fmt"
	"strings"

	"github.com/go-ldap/ldap/v3"
)

// CreatePerson creates a new person in LDAP.
func (c *Client) CreatePerson(uid, displayName, email, firstName, lastName string) (*Person, error) {
	uid = normalizeUID(uid)
	if err := validateIdentityName("username", uid); err != nil {
		return nil, err
	}
	dn := fmt.Sprintf("uid=%s,ou=people,%s", escapeDNComponent(uid), c.baseDN)

	if _, err := c.GetPerson(uid); err == nil {
		return nil, fmt.Errorf("person %q already exists", uid)
	}

	addReq := ldap.NewAddRequest(dn, nil)
	addReq.Attribute("objectClass", []string{"inetOrgPerson", "posixAccount", "shadowAccount"})
	addReq.Attribute("uid", []string{uid})
	addReq.Attribute("cn", []string{displayName})
	addReq.Attribute("sn", []string{lastName})
	addReq.Attribute("givenName", []string{firstName})
	addReq.Attribute("displayName", []string{displayName})
	addReq.Attribute("mail", []string{email})
	addReq.Attribute("uidNumber", []string{fmt.Sprintf("%d", 10000+hashUID(uid))})
	addReq.Attribute("gidNumber", []string{"10000"})
	addReq.Attribute("homeDirectory", []string{fmt.Sprintf("/home/%s", uid)})
	addReq.Attribute("loginShell", []string{"/bin/bash"})
	addReq.Attribute("userPassword", []string{"TempPass123!"})
	addReq.Attribute("shadowExpire", []string{"-1"})

	if err := c.do(func(conn *ldap.Conn) error {
		return conn.Add(addReq)
	}); err != nil {
		return nil, fmt.Errorf("creating person: %w", err)
	}

	return c.GetPerson(uid)
}

// GetPerson retrieves a person by UID.
func (c *Client) GetPerson(uid string) (*Person, error) {
	uid = normalizeUID(uid)
	if err := validateIdentityName("username", uid); err != nil {
		return nil, err
	}
	filter := fmt.Sprintf("(uid=%s)", escapeFilter(uid))
	searchBase := fmt.Sprintf("ou=people,%s", c.baseDN)

	searchReq := ldap.NewSearchRequest(
		searchBase,
		ldap.ScopeWholeSubtree, ldap.NeverDerefAliases, 1, 0, false,
		filter,
		[]string{"dn", "uid", "displayName", "mail", "givenName", "sn", "memberOf", "shadowExpire"},
		nil,
	)

	var result *ldap.SearchResult
	if err := c.do(func(conn *ldap.Conn) error {
		var err error
		result, err = conn.Search(searchReq)
		return err
	}); err != nil {
		return nil, fmt.Errorf("searching for person: %w", err)
	}

	if len(result.Entries) == 0 {
		return nil, fmt.Errorf("person %q not found", uid)
	}

	entry := result.Entries[0]
	return &Person{
		DN:          entry.DN,
		UID:         entry.GetAttributeValue("uid"),
		DisplayName: entry.GetAttributeValue("displayName"),
		Email:       entry.GetAttributeValue("mail"),
		FirstName:   entry.GetAttributeValue("givenName"),
		LastName:    entry.GetAttributeValue("sn"),
		Groups:      shortNames(entry.GetAttributeValues("memberOf")),
		Enabled:     isPersonEnabled(entry),
	}, nil
}

// shortNames converts a list of DNs/CNs into short names.
func shortNames(values []string) []string {
	result := make([]string, 0, len(values))
	for _, v := range values {
		if v == "" {
			continue
		}
		v = strings.ToLower(v)
		if strings.HasPrefix(v, "cn=") {
			parts := strings.SplitN(v, ",", 2)
			result = append(result, strings.TrimPrefix(parts[0], "cn="))
		} else if strings.HasPrefix(v, "uid=") {
			parts := strings.SplitN(v, ",", 2)
			result = append(result, strings.TrimPrefix(parts[0], "uid="))
		} else {
			result = append(result, v)
		}
	}
	return result
}

// ListPeople returns all persons.
func (c *Client) ListPeople() ([]Person, error) {
	searchBase := fmt.Sprintf("ou=people,%s", c.baseDN)
	filter := "(objectClass=inetOrgPerson)"

	searchReq := ldap.NewSearchRequest(
		searchBase,
		ldap.ScopeWholeSubtree, ldap.NeverDerefAliases, 0, 0, false,
		filter,
		[]string{"uid", "displayName", "mail", "givenName", "sn", "memberOf", "shadowExpire"},
		nil,
	)

	var result *ldap.SearchResult
	if err := c.do(func(conn *ldap.Conn) error {
		var err error
		result, err = conn.Search(searchReq)
		return err
	}); err != nil {
		return nil, fmt.Errorf("listing people: %w", err)
	}

	var people []Person = []Person{}
	for _, entry := range result.Entries {
		people = append(people, Person{
			DN:          entry.DN,
			UID:         entry.GetAttributeValue("uid"),
			DisplayName: entry.GetAttributeValue("displayName"),
			Email:       entry.GetAttributeValue("mail"),
			FirstName:   entry.GetAttributeValue("givenName"),
			LastName:    entry.GetAttributeValue("sn"),
			Groups:      shortNames(entry.GetAttributeValues("memberOf")),
			Enabled:     isPersonEnabled(entry),
		})
	}

	return people, nil
}

// isPersonEnabled returns true if the account is not expired.
// shadowExpire == "-1" means never expires; empty means not set (enabled);
// a non-zero, non-"-1" value is days since epoch — 0 means expired.
func isPersonEnabled(entry *ldap.Entry) bool {
	v := entry.GetAttributeValue("shadowExpire")
	if v == "" || v == "-1" {
		return true
	}
	return v != "0"
}
