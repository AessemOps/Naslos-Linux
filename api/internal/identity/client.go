// Package identity provides LDAP-based user and group management for Naslos.
// All password changes flow through SetPassword to keep LDAP and SMB in sync.
package identity

import (
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/go-ldap/ldap/v3"
)

// Client is an LDAP client for identity operations.
type Client struct {
	conn       *ldap.Conn
	baseDN     string
	bindDN     string
	bindPass   string
	tlsConfig  *tls.Config
}

// Config holds LDAP connection configuration.
type Config struct {
	Host       string
	Port       int
	BaseDN     string
	BindDN     string
	BindPass   string
	CACertPath string
	UseTLS     bool
}

// NewClient creates a new identity client.
func NewClient(cfg Config) (*Client, error) {
	addr := fmt.Sprintf("%s:%d", cfg.Host, cfg.Port)

	var tlsConfig *tls.Config
	if cfg.UseTLS {
		tlsConfig = &tls.Config{
			ServerName: cfg.Host,
			MinVersion: tls.VersionTLS12,
		}
		if cfg.CACertPath != "" {
			caCert, err := os.ReadFile(cfg.CACertPath)
			if err != nil {
				return nil, fmt.Errorf("reading CA cert: %w", err)
			}
			caPool := x509.NewCertPool()
			if !caPool.AppendCertsFromPEM(caCert) {
				return nil, fmt.Errorf("failed to parse CA certificate")
			}
			tlsConfig.RootCAs = caPool
		}
	}

	c := &Client{
		baseDN:    cfg.BaseDN,
		bindDN:    cfg.BindDN,
		bindPass:  cfg.BindPass,
		tlsConfig: tlsConfig,
	}

	if err := c.connect(addr); err != nil {
		return nil, err
	}

	return c, nil
}

// connect establishes the LDAP connection.
func (c *Client) connect(addr string) error {
	var conn *ldap.Conn
	var err error

	if c.tlsConfig != nil {
		conn, err = ldap.DialTLS("tcp", addr, c.tlsConfig)
	} else {
		conn, err = ldap.Dial("tcp", addr)
	}
	if err != nil {
		return fmt.Errorf("connecting to LDAP: %w", err)
	}

	if err := conn.Bind(c.bindDN, c.bindPass); err != nil {
		conn.Close()
		return fmt.Errorf("binding to LDAP: %w", err)
	}

	c.conn = conn
	return nil
}

// Close closes the LDAP connection.
func (c *Client) Close() error {
	if c.conn != nil {
		return c.conn.Close()
	}
	return nil
}

// reconnect attempts to re-establish the connection.
func (c *Client) reconnect() error {
	if c.conn != nil {
		c.conn.Close()
	}
	addr := fmt.Sprintf("%s:%d", extractHost(c.bindDN), 636)
	return c.connect(addr)
}

func extractHost(bindDN string) string {
	// Simple extraction - in production, store host separately
	return "naslos-openldap"
}

// Person represents a user account.
type Person struct {
	DN          string    `json:"dn"`
	UID         string    `json:"uid"`
	DisplayName string    `json:"displayName"`
	Email       string    `json:"email"`
	FirstName   string    `json:"firstName"`
	LastName    string    `json:"lastName"`
	Enabled     bool      `json:"enabled"`
	Groups      []string  `json:"groups"`
	CreatedAt   time.Time `json:"createdAt"`
}

// Group represents a group.
type Group struct {
	DN          string   `json:"dn"`
	CN          string   `json:"cn"`
	Description string   `json:"description"`
	Members     []string `json:"members"`
}

// base64Encode encodes bytes to base64 string.
func base64Encode(data []byte) string {
	return base64.StdEncoding.EncodeToString(data)
}

// normalizeUID normalizes a username for LDAP.
func normalizeUID(uid string) string {
	return strings.ToLower(strings.TrimSpace(uid))
}
