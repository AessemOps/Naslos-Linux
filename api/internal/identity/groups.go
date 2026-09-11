package identity

import (
	"fmt"
	"strings"

	"github.com/go-ldap/ldap/v3"
)

// CreateGroup creates a new group. The description attribute is optional
// and omitted when empty (OpenLDAP rejects empty string values).
func (c *Client) CreateGroup(cn, description string) (*Group, error) {
	dn := fmt.Sprintf("cn=%s,ou=groups,%s", cn, c.baseDN)

	addReq := ldap.NewAddRequest(dn, nil)
	addReq.Attribute("objectClass", []string{"groupOfNames"})
	addReq.Attribute("cn", []string{cn})
	if description != "" {
		addReq.Attribute("description", []string{description})
	}
	// Add placeholder member to satisfy groupOfNames schema
	addReq.Attribute("member", []string{c.placeholderMemberDN()})

	if err := c.conn.Add(addReq); err != nil {
		return nil, fmt.Errorf("creating group: %w", err)
	}

	return c.GetGroup(cn)
}

// placeholderMemberDN returns the DN used to satisfy the groupOfNames schema
// requirement of at least one member. It is filtered from API responses.
func (c *Client) placeholderMemberDN() string {
	return fmt.Sprintf("cn=empty-members,ou=groups,%s", c.baseDN)
}

// filterPlaceholderMembers removes the schema-required placeholder member from
// the returned member list.
func (c *Client) filterPlaceholderMembers(members []string) []string {
	var filtered []string = []string{}
	for _, m := range members {
		if strings.HasPrefix(m, "cn=empty-members,") {
			continue
		}
		filtered = append(filtered, m)
	}
	return filtered
}

// GetGroup retrieves a group by CN.
func (c *Client) GetGroup(cn string) (*Group, error) {
	filter := fmt.Sprintf("(cn=%s)", cn)
	searchBase := fmt.Sprintf("ou=groups,%s", c.baseDN)

	searchReq := ldap.NewSearchRequest(
		searchBase,
		ldap.ScopeWholeSubtree, ldap.NeverDerefAliases, 1, 0, false,
		filter,
		[]string{"dn", "cn", "description", "member"},
		nil,
	)

	result, err := c.conn.Search(searchReq)
	if err != nil {
		return nil, fmt.Errorf("searching for group: %w", err)
	}

	if len(result.Entries) == 0 {
		return nil, fmt.Errorf("group %q not found", cn)
	}

	entry := result.Entries[0]
	return &Group{
		DN:          entry.DN,
		CN:          entry.GetAttributeValue("cn"),
		Description: entry.GetAttributeValue("description"),
		Members:     c.filterPlaceholderMembers(entry.GetAttributeValues("member")),
	}, nil
}

// ListGroups returns all groups.
func (c *Client) ListGroups() ([]Group, error) {
	searchBase := fmt.Sprintf("ou=groups,%s", c.baseDN)
	filter := "(objectClass=groupOfNames)"

	searchReq := ldap.NewSearchRequest(
		searchBase,
		ldap.ScopeWholeSubtree, ldap.NeverDerefAliases, 0, 0, false,
		filter,
		[]string{"cn", "description", "member"},
		nil,
	)

	result, err := c.conn.Search(searchReq)
	if err != nil {
		return nil, fmt.Errorf("listing groups: %w", err)
	}

	var groups []Group = []Group{}
	for _, entry := range result.Entries {
		groups = append(groups, Group{
			DN:          entry.DN,
			CN:          entry.GetAttributeValue("cn"),
			Description: entry.GetAttributeValue("description"),
			Members:     c.filterPlaceholderMembers(entry.GetAttributeValues("member")),
		})
	}

	return groups, nil
}

// AddMember adds a person to a group.
func (c *Client) AddMember(groupCN, personUID string) error {
	dn := fmt.Sprintf("cn=%s,ou=groups,%s", groupCN, c.baseDN)
	personDN := fmt.Sprintf("uid=%s,ou=people,%s", normalizeUID(personUID), c.baseDN)

	modReq := ldap.NewModifyRequest(dn, nil)
	modReq.Add("member", []string{personDN})
	return c.conn.Modify(modReq)
}

// RemoveMember removes a person from a group.
func (c *Client) RemoveMember(groupCN, personUID string) error {
	dn := fmt.Sprintf("cn=%s,ou=groups,%s", groupCN, c.baseDN)
	personDN := fmt.Sprintf("uid=%s,ou=people,%s", normalizeUID(personUID), c.baseDN)

	modReq := ldap.NewModifyRequest(dn, nil)
	modReq.Delete("member", []string{personDN})
	return c.conn.Modify(modReq)
}

// DeleteGroup removes a group.
func (c *Client) DeleteGroup(cn string) error {
	dn := fmt.Sprintf("cn=%s,ou=groups,%s", cn, c.baseDN)
	delReq := ldap.NewDelRequest(dn, nil)
	return c.conn.Del(delReq)
}

// GetPersonGroups returns all groups a person belongs to.
func (c *Client) GetPersonGroups(uid string) ([]string, error) {
	groups, err := c.ListGroups()
	if err != nil {
		return nil, err
	}

	var memberships []string
	uid = normalizeUID(uid)
	personDN := fmt.Sprintf("uid=%s,ou=people,%s", uid, c.baseDN)

	for _, g := range groups {
		for _, member := range g.Members {
			if member == personDN {
				memberships = append(memberships, g.CN)
				break
			}
		}
	}

	return memberships, nil
}
