package identity

import (
	"fmt"

	"github.com/go-ldap/ldap/v3"
)

// CreatePerson creates a new person in LDAP.
func (c *Client) CreatePerson(uid, displayName, email, firstName, lastName string) (*Person, error) {
	uid = normalizeUID(uid)
	dn := fmt.Sprintf("uid=%s,ou=people,%s", uid, c.baseDN)

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

	if err := c.conn.Add(addReq); err != nil {
		return nil, fmt.Errorf("creating person: %w", err)
	}

	return c.GetPerson(uid)
}

// GetPerson retrieves a person by UID.
func (c *Client) GetPerson(uid string) (*Person, error) {
	uid = normalizeUID(uid)
	filter := fmt.Sprintf("(uid=%s)", uid)
	searchBase := fmt.Sprintf("ou=people,%s", c.baseDN)

	searchReq := ldap.NewSearchRequest(
		searchBase,
		ldap.ScopeWholeSubtree, ldap.NeverDerefAliases, 1, 0, false,
		filter,
		[]string{"dn", "uid", "displayName", "mail", "givenName", "sn", "memberOf", "shadowExpire"},
		nil,
	)

	result, err := c.conn.Search(searchReq)
	if err != nil {
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
		Groups:      entry.GetAttributeValues("memberOf"),
		Enabled:     entry.GetAttributeValue("shadowExpire") == "-1",
	}, nil
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

	result, err := c.conn.Search(searchReq)
	if err != nil {
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
			Groups:      entry.GetAttributeValues("memberOf"),
			Enabled:     entry.GetAttributeValue("shadowExpire") == "-1",
		})
	}

	return people, nil
}
