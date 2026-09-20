// Package identity provides LDAP-based user and group management for Naslos.
// All password changes flow through SetPassword to keep LDAP and SMB in sync.
package identity

import (
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/go-ldap/ldap/v3"
)

const (
	// connectTimeout bounds a single dial+bind attempt.
	connectTimeout = 5 * time.Second
	// connectCooldown throttles reconnect attempts so that a burst of page
	// requests does not each block on a full dial timeout while LDAP is down.
	connectCooldown = 2 * time.Second
)

// Client is an LDAP client for identity operations.
//
// The connection is established lazily on first use and re-established
// automatically after failure, so the API tolerates LDAP being unavailable at
// startup (e.g. both pods restarting together) as well as LDAP restarts at
// runtime. Never dial from a constructor — use EnsureConnection or do().
type Client struct {
	mu        sync.Mutex
	conn      *ldap.Conn
	addr      string
	baseDN    string
	bindDN    string
	bindPass  string
	tlsConfig *tls.Config

	// lastErr/lastAttempt back the reconnect cooldown.
	lastErr     error
	lastAttempt time.Time
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

// NewClient creates an identity client. It validates the configuration but
// deliberately does NOT dial LDAP: the connection is established lazily by
// EnsureConnection on first use. This keeps API startup independent of LDAP
// availability, which is what makes restart races survivable.
func NewClient(cfg Config) (*Client, error) {
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

	return &Client{
		addr:      fmt.Sprintf("%s:%d", cfg.Host, cfg.Port),
		baseDN:    cfg.BaseDN,
		bindDN:    cfg.BindDN,
		bindPass:  cfg.BindPass,
		tlsConfig: tlsConfig,
	}, nil
}

// EnsureConnection guarantees a live, bound LDAP connection, dialing on first
// use and re-dialing after any failure. Failures are throttled by
// connectCooldown so callers get a fast error instead of stalling.
func (c *Client) EnsureConnection() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.ensureLocked()
}

// ensureLocked implements EnsureConnection; c.mu must be held.
func (c *Client) ensureLocked() error {
	if c.conn != nil && !c.conn.IsClosing() {
		return nil
	}
	if c.conn != nil {
		c.conn.Close()
		c.conn = nil
	}
	if c.lastErr != nil && time.Since(c.lastAttempt) < connectCooldown {
		return c.lastErr
	}
	c.lastAttempt = time.Now()
	if err := c.connectLocked(); err != nil {
		c.conn = nil
		c.lastErr = err
		return err
	}
	c.lastErr = nil
	return nil
}

// connectLocked dials and binds; c.mu must be held.
func (c *Client) connectLocked() error {
	dialer := &net.Dialer{Timeout: connectTimeout}

	var conn *ldap.Conn
	var err error
	if c.tlsConfig != nil {
		conn, err = ldap.DialURL("ldaps://"+c.addr,
			ldap.DialWithDialer(dialer),
			ldap.DialWithTLSConfig(c.tlsConfig))
	} else {
		conn, err = ldap.DialURL("ldap://"+c.addr, ldap.DialWithDialer(dialer))
	}
	if err != nil {
		return fmt.Errorf("connecting to LDAP %s: %w", c.addr, err)
	}

	conn.SetTimeout(connectTimeout)

	if err := conn.Bind(c.bindDN, c.bindPass); err != nil {
		conn.Close()
		return fmt.Errorf("binding to LDAP %s as %s: %w", c.addr, c.bindDN, err)
	}

	c.conn = conn
	return nil
}

// do runs fn against a live connection. If the connection turns out to be dead
// (for example the OpenLDAP pod restarted), it is dropped and fn is retried
// once on a fresh connection.
func (c *Client) do(fn func(conn *ldap.Conn) error) error {
	if err := c.EnsureConnection(); err != nil {
		return err
	}

	conn := c.currentConn()
	if conn == nil {
		return errors.New("LDAP connection is unavailable")
	}

	err := fn(conn)
	if err == nil || !isConnectionError(err) {
		return err
	}

	c.drop(conn)
	if reconErr := c.EnsureConnection(); reconErr != nil {
		return err
	}
	retryConn := c.currentConn()
	if retryConn == nil {
		return err
	}
	return fn(retryConn)
}

// currentConn returns the active connection, if any.
func (c *Client) currentConn() *ldap.Conn {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.conn
}

// drop invalidates conn if it is still the active connection.
func (c *Client) drop(conn *ldap.Conn) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.conn == conn {
		c.conn.Close()
		c.conn = nil
	}
}

// isConnectionError reports whether err means the connection itself failed, as
// opposed to a protocol-level rejection (e.g. "no such object") that a retry
// cannot fix.
func isConnectionError(err error) bool {
	if err == nil {
		return false
	}

	var ldapErr *ldap.Error
	if errors.As(err, &ldapErr) {
		switch ldapErr.ResultCode {
		case ldap.ErrorNetwork,
			ldap.ErrorUnexpectedResponse,
			ldap.LDAPResultUnavailable,
			ldap.LDAPResultBusy,
			ldap.LDAPResultServerDown:
			return true
		}
	}

	var netErr net.Error
	if errors.As(err, &netErr) {
		return true
	}

	if errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) ||
		errors.Is(err, net.ErrClosed) {
		return true
	}

	msg := strings.ToLower(err.Error())
	for _, fragment := range []string{
		"connection reset",
		"broken pipe",
		"use of closed network connection",
		"response channel closed",
		"connection refused",
		"no such host",
		"ldap: connection closed",
	} {
		if strings.Contains(msg, fragment) {
			return true
		}
	}
	return false
}

// Close closes the LDAP connection.
func (c *Client) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.conn != nil {
		err := c.conn.Close()
		c.conn = nil
		return err
	}
	return nil
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

// normalizeUID normalizes a username for LDAP.
func normalizeUID(uid string) string {
	return strings.ToLower(strings.TrimSpace(uid))
}
