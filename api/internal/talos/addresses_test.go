package talos

import (
	"net/netip"
	"testing"

	"github.com/siderolabs/talos/pkg/machinery/resources/network"
)

func addressSpec(link, prefix string) *network.AddressStatusSpec {
	return &network.AddressStatusSpec{
		LinkName: link,
		Address:  netip.MustParsePrefix(prefix),
	}
}

// TestAddressesByLink pins FR-MET-10's selection rules: the address shown for an
// interface is the one an operator would use - routable IPv4 first - not an
// IPv6 link-local or the loopback address.
func TestAddressesByLink(t *testing.T) {
	specs := []*network.AddressStatusSpec{
		addressSpec("lo", "127.0.0.1/8"),
		addressSpec("eth0", "fe80::1/64"),       // link-local: never chosen
		addressSpec("eth0", "2001:db8::5/64"),   // routable v6
		addressSpec("eth0", "192.168.1.96/24"),  // routable v4: wins
		addressSpec("eth1", "2001:db8::6/64"),   // v6 only
		addressSpec("eth2", "169.254.10.10/16"), // link-local v4
		addressSpec("", "10.0.0.1/8"),           // no link name
		addressSpec("eth3", "224.0.0.1/4"),      // multicast
	}

	got := addressesByLink(specs)

	if got["eth0"] != "192.168.1.96" {
		t.Errorf("eth0 = %q, want the routable IPv4 address", got["eth0"])
	}
	if got["eth1"] != "2001:db8::6" {
		t.Errorf("eth1 = %q, want the routable IPv6 address", got["eth1"])
	}
	for _, link := range []string{"lo", "eth2", "", "eth3"} {
		if addr, ok := got[link]; ok {
			t.Errorf("%q = %q, want no address chosen", link, addr)
		}
	}
}

// TestAddressesByLinkPrefersIPv4OverIPv6OrAPreviouslySeenAddress keeps the choice
// deterministic when a link has several addresses.
func TestAddressesByLinkPrefersIPv4OverIPv6(t *testing.T) {
	specs := []*network.AddressStatusSpec{
		addressSpec("eth0", "2001:db8::5/64"),
		addressSpec("eth0", "10.10.10.5/24"),
		addressSpec("eth0", "10.10.10.6/24"),
	}
	if got := addressesByLink(specs)["eth0"]; got != "10.10.10.5" {
		t.Errorf("eth0 = %q, want the first IPv4 address", got)
	}
}
