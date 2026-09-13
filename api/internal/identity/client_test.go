package identity

import (
	"net"
	"strconv"
	"strings"
	"testing"
	"time"
)

// closedPort returns a port that is guaranteed to have nothing listening.
func closedPort(t *testing.T) int {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	port := ln.Addr().(*net.TCPAddr).Port
	ln.Close()
	return port
}

// splitHostPort returns the raw host and port components.
func splitHostPort(t *testing.T, host string, port int) (string, int) {
	t.Helper()
	if _, err := strconv.Atoi(strconv.Itoa(port)); err != nil {
		t.Fatalf("bad port %d: %v", port, err)
	}
	return host, port
}

// TestNewClientIsLazyAndDoesNotFailWhenLDAPIsDown locks in the restart-race
// fix: constructing a client must not require LDAP to be reachable. Before the
// fix, NewClient dialed eagerly and callers (server.New) turned any failure
// into a permanent nil client, which is what made the Users/Groups pages fail
// with "Identity/LDAP is not available" until the API pod was restarted.
func TestNewClientIsLazyAndDoesNotFailWhenLDAPIsDown(t *testing.T) {
	cfg := Config{
		Host:     "127.0.0.1",
		Port:     closedPort(t),
		BaseDN:   "dc=naslos,dc=local",
		BindDN:   "cn=naslos-service,ou=services,dc=naslos,dc=local",
		BindPass: "secret",
		UseTLS:   false,
	}

	client, err := NewClient(cfg)
	if err != nil {
		t.Fatalf("NewClient must not fail when LDAP is unreachable, got: %v", err)
	}
	if client == nil {
		t.Fatal("NewClient returned a nil client")
	}
	defer client.Close()

	// Operations must report a clear error rather than panic on a nil conn.
	if _, err := client.ListGroups(); err == nil {
		t.Fatal("expected an error when LDAP is unreachable")
	} else if !strings.Contains(err.Error(), "127.0.0.1") {
		t.Fatalf("error should identify the LDAP address, got: %v", err)
	}

	// And the client must remain usable (not latched dead): a later call
	// attempts to connect again instead of short-circuiting forever.
	if err := client.EnsureConnection(); err == nil {
		t.Fatal("expected EnsureConnection to fail while LDAP is down")
	}
}

// TestEnsureConnectionRetriesAfterFailure verifies that a failed connect does
// not permanently poison the client: once LDAP becomes reachable, the next
// attempt succeeds. A raw TCP listener stands in for LDAP — it is enough to
// prove the dial is retried (the bind then fails, which is fine here).
func TestEnsureConnectionRetriesAfterFailure(t *testing.T) {
	// 1) Nothing listening yet: first attempt fails.
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	addr := ln.Addr().(*net.TCPAddr)
	ln.Close() // free the port so the initial connect fails

	host, portStr := splitHostPort(t, addr.IP.String(), addr.Port)
	client, err := NewClient(Config{
		Host:     host,
		Port:     portStr,
		BaseDN:   "dc=naslos,dc=local",
		BindDN:   "cn=svc,dc=naslos,dc=local",
		BindPass: "secret",
	})
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	defer client.Close()

	if err := client.EnsureConnection(); err == nil {
		t.Fatal("expected the first connect attempt to fail")
	}

	// 2) LDAP "comes back": the same client must be able to reach it again.
	ln2, err := net.Listen("tcp", addr.String())
	if err != nil {
		t.Fatalf("re-listen: %v", err)
	}
	defer ln2.Close()

	// Accept and immediately close so the LDAP bind fails fast; the point is
	// that a *dial* is attempted again rather than the failure being cached
	// permanently.
	go func() {
		for {
			conn, err := ln2.Accept()
			if err != nil {
				return
			}
			conn.Close()
		}
	}()

	// connectCooldown throttles retries; wait it out.
	time.Sleep(connectCooldown + 100*time.Millisecond)

	err = client.EnsureConnection()
	if err == nil {
		// Unexpected but acceptable: a bind to a closed connection could race.
		t.Log("connection unexpectedly succeeded; dial retry proven")
		return
	}
	if strings.Contains(err.Error(), "connect: connection refused") {
		t.Fatalf("client did not retry the dial after the failure: %v", err)
	}
	if !strings.Contains(err.Error(), "binding to LDAP") {
		t.Fatalf("expected a bind failure (proving a fresh dial happened), got: %v", err)
	}
}
