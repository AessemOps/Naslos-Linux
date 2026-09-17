// Package talos provides a client for the Talos Linux API.
package talos

import (
	"context"
	"fmt"
	"io"
	"strings"
	"time"

	"github.com/cosi-project/runtime/pkg/safe"
	"github.com/siderolabs/talos/pkg/machinery/api/machine"
	"github.com/siderolabs/talos/pkg/machinery/client"
	clientconfig "github.com/siderolabs/talos/pkg/machinery/client/config"
	"github.com/siderolabs/talos/pkg/machinery/resources/network"
)

// Client wraps the Talos API client.
type Client struct {
	client *client.Client
	ctx    context.Context
}

// NewClient creates a new Talos client from a talosconfig file.
// If talosConfig is empty, it uses the default config path (~/.talos/config).
func NewClient(ctx context.Context, talosConfig string) (*Client, error) {
	cfg, err := loadConfig(talosConfig)
	if err != nil {
		return nil, fmt.Errorf("loading talosconfig: %w", err)
	}

	c, err := client.New(ctx,
		client.WithConfig(cfg),
	)
	if err != nil {
		return nil, fmt.Errorf("creating Talos client: %w", err)
	}

	return &Client{
		client: c,
		ctx:    ctx,
	}, nil
}

// loadConfig loads a talosconfig from the given path or the default location.
//
// When path is empty, this delegates to clientconfig.Open("") so the
// machinery library's own default-path resolution runs: it checks the
// TALOSCONFIG environment variable first, then ~/.talos/config, then the
// in-cluster service-account mount (/var/run/secrets/talos.dev/config).
// A previous version of this function hardcoded ~/.talos/config directly,
// which silently skipped the TALOSCONFIG env var and made the API pod fail
// at startup with "failed to determine endpoints" even when a talosconfig
// was mounted and TALOSCONFIG was set.
func loadConfig(path string) (*clientconfig.Config, error) {
	return clientconfig.Open(path)
}

// Close closes the Talos client connection.
func (c *Client) Close() error {
	return c.client.Close()
}

// SystemMetrics holds system-level metrics collected from Talos.
type SystemMetrics struct {
	CPU     CPUMetrics     `json:"cpu"`
	Memory  MemoryMetrics  `json:"memory"`
	Disk    DiskMetrics    `json:"disk"`
	Network NetworkMetrics `json:"network"`
	System  SystemInfo     `json:"system"`
}

// CPUMetrics holds CPU usage metrics.
type CPUMetrics struct {
	UsagePercent float64 `json:"usagePercent"`
	Cores        int     `json:"cores"`
	LoadAvg1     float64 `json:"loadAvg1"`
	LoadAvg5     float64 `json:"loadAvg5"`
	LoadAvg15    float64 `json:"loadAvg15"`
}

// MemoryMetrics holds memory usage metrics.
type MemoryMetrics struct {
	Total        uint64  `json:"total"`
	Used         uint64  `json:"used"`
	Free         uint64  `json:"free"`
	Available    uint64  `json:"available"`
	UsagePercent float64 `json:"usagePercent"`
	SwapTotal    uint64  `json:"swapTotal"`
	SwapUsed     uint64  `json:"swapUsed"`
}

// DiskMetrics holds disk usage metrics for the root filesystem.
type DiskMetrics struct {
	Total        uint64  `json:"total"`
	Used         uint64  `json:"used"`
	Free         uint64  `json:"free"`
	UsagePercent float64 `json:"usagePercent"`
}

// NetworkMetrics holds network interface metrics.
type NetworkMetrics struct {
	BytesSent   uint64             `json:"bytesSent"`
	BytesRecv   uint64             `json:"bytesRecv"`
	PacketsSent uint64             `json:"packetsSent"`
	PacketsRecv uint64             `json:"packetsRecv"`
	Interfaces  []NetworkInterface `json:"interfaces"`
}

// NetworkInterface holds per-interface network stats.
type NetworkInterface struct {
	Name      string `json:"name"`
	IPAddress string `json:"ipAddress"`
}

// SystemInfo holds general system information.
type SystemInfo struct {
	Hostname     string `json:"hostname"`
	Uptime       uint64 `json:"uptime"`
	OS           string `json:"os"`
	Kernel       string `json:"kernel"`
	TalosVersion string `json:"talosVersion"`
}

// getInterfaceAddresses maps each link to its primary address. /proc/net/dev has
// the counters but no addresses, so this reads the node's AddressStatus resources
// through the Talos API - the same data `talosctl get addresses` shows. A failure
// is not fatal: the dashboard shows interface names without addresses rather than
// no metrics at all (FR-MET-10).
func (c *Client) getInterfaceAddresses() map[string]string {
	ctx, cancel := context.WithTimeout(c.ctx, 5*time.Second)
	defer cancel()

	list, err := safe.StateListAll[*network.AddressStatus](ctx, c.client.COSI)
	if err != nil {
		return nil
	}

	specs := make([]*network.AddressStatusSpec, 0, list.Len())
	for status := range list.All() {
		specs = append(specs, status.TypedSpec())
	}
	return addressesByLink(specs)
}

// addressesByLink picks one address per link: IPv4 over IPv6 (what an operator
// types to reach the node), routable over link-local, and no loopback.
func addressesByLink(specs []*network.AddressStatusSpec) map[string]string {
	out := make(map[string]string, len(specs))
	best := make(map[string]int, len(specs))

	for _, spec := range specs {
		if spec == nil || spec.LinkName == "" {
			continue
		}
		addr := spec.Address.Addr()
		if !addr.IsValid() || addr.IsLoopback() || addr.IsLinkLocalUnicast() || addr.IsMulticast() {
			continue
		}
		score := 0
		if addr.Is4() || addr.Is4In6() {
			score = 1
		}
		if current, seen := best[spec.LinkName]; seen && current >= score {
			continue
		}
		out[spec.LinkName] = addr.Unmap().String()
		best[spec.LinkName] = score
	}
	return out
}

// GetSystemMetrics collects CPU, memory, disk, network, and system info
// from the Talos node via the Talos controller-gen / memory resources.
func (c *Client) GetSystemMetrics() (SystemMetrics, error) {
	metrics := SystemMetrics{}

	// System info from Talos
	if info, err := c.getSystemInfo(); err == nil {
		metrics.System = info
	}

	// Memory metrics from Talos
	if mem, err := c.getMemoryMetrics(); err == nil {
		metrics.Memory = mem
	}

	// CPU metrics from Talos
	if cpu, err := c.getCPUMetrics(); err == nil {
		metrics.CPU = cpu
	}

	// Disk metrics from Talos
	if disk, err := c.getDiskMetrics(); err == nil {
		metrics.Disk = disk
	}

	// Network metrics from Talos
	if net, err := c.getNetworkMetrics(); err == nil {
		metrics.Network = net
	}

	return metrics, nil
}

// getSystemInfo collects hostname, uptime, OS, and Talos version from the Talos node.
func (c *Client) getSystemInfo() (SystemInfo, error) {
	info := SystemInfo{OS: "Talos Linux"}

	ver, err := c.client.Version(c.ctx)
	if err != nil {
		return info, fmt.Errorf("getting version: %w", err)
	}
	for _, m := range ver.Messages {
		if m.GetVersion() != nil {
			info.TalosVersion = m.GetVersion().GetTag()
		}
		if m.GetPlatform() != nil {
			info.OS = m.GetPlatform().GetName()
		}
		break
	}

	if r, err := c.client.Read(c.ctx, "proc/uptime"); err == nil {
		data, _ := io.ReadAll(r)
		r.Close()
		parts := strings.Fields(strings.TrimSpace(string(data)))
		if len(parts) > 0 {
			if f, err := parseFloat(parts[0]); err == nil {
				info.Uptime = uint64(f)
			}
		}
	}

	if r, err := c.client.Read(c.ctx, "proc/sys/kernel/hostname"); err == nil {
		data, _ := io.ReadAll(r)
		r.Close()
		info.Hostname = strings.TrimSpace(string(data))
	}

	if r, err := c.client.Read(c.ctx, "proc/sys/kernel/osrelease"); err == nil {
		data, _ := io.ReadAll(r)
		r.Close()
		info.Kernel = strings.TrimSpace(string(data))
	}

	return info, nil
}

// getMemoryMetrics collects memory and swap usage from the Talos node.
func (c *Client) getMemoryMetrics() (MemoryMetrics, error) {
	mm := MemoryMetrics{}
	resp, err := c.client.Memory(c.ctx)
	if err != nil {
		return mm, fmt.Errorf("getting memory: %w", err)
	}
	for _, m := range resp.Messages {
		if mi := m.GetMeminfo(); mi != nil {
			// MemInfo values mirror /proc/meminfo and are reported in KB;
			// convert to bytes so all API quantities are byte-denominated.
			const kb = 1024
			mm.Total = mi.GetMemtotal() * kb
			mm.Free = mi.GetMemfree() * kb
			mm.Available = mi.GetMemavailable() * kb
			mm.Used = mm.Total - mm.Available
			mm.SwapTotal = mi.GetSwaptotal() * kb
			swapFree := mi.GetSwapfree() * kb
			if mm.SwapTotal >= swapFree {
				mm.SwapUsed = mm.SwapTotal - swapFree
			}
		}
		break
	}
	if mm.Total > 0 {
		mm.UsagePercent = float64(mm.Used) / float64(mm.Total) * 100
	}
	return mm, nil
}

// getCPUMetrics collects CPU core count and load average from the Talos node.
func (c *Client) getCPUMetrics() (CPUMetrics, error) {
	cpu := CPUMetrics{}

	if r, err := c.client.Read(c.ctx, "proc/cpuinfo"); err == nil {
		data, _ := io.ReadAll(r)
		r.Close()
		for _, line := range strings.Split(string(data), "\n") {
			if strings.HasPrefix(line, "processor") {
				cpu.Cores++
			}
		}
	}

	if r, err := c.client.Read(c.ctx, "proc/loadavg"); err == nil {
		data, _ := io.ReadAll(r)
		r.Close()
		f := strings.Fields(strings.TrimSpace(string(data)))
		if len(f) >= 3 {
			if v, err := parseFloat(f[0]); err == nil {
				cpu.LoadAvg1 = v
			}
			if v, err := parseFloat(f[1]); err == nil {
				cpu.LoadAvg5 = v
			}
			if v, err := parseFloat(f[2]); err == nil {
				cpu.LoadAvg15 = v
			}
		}
	}

	if cpu.LoadAvg1 > 0 && cpu.Cores > 0 {
		cpu.UsagePercent = (cpu.LoadAvg1 / float64(cpu.Cores)) * 100
		if cpu.UsagePercent > 100 {
			cpu.UsagePercent = 100
		}
	}
	return cpu, nil
}

// getDiskMetrics collects root filesystem usage from the Talos node.
// The Machine.Mounts RPC reports statfs data (size + available) per mount,
// while DiskUsage only reports total size and cannot yield used/free.
// On Talos, "/" is a read-only squashfs image, so the writable state lives
// on the EPHEMERAL partition mounted at /var; prefer it and fall back to /.
func (c *Client) getDiskMetrics() (DiskMetrics, error) {
	dm := DiskMetrics{}

	resp, err := c.client.Mounts(c.ctx)
	if err != nil {
		return dm, fmt.Errorf("getting mounts: %w", err)
	}

	pick := func(st *machine.MountStat) (DiskMetrics, bool) {
		var out DiskMetrics
		out.Total = st.GetSize()
		out.Free = st.GetAvailable()
		if out.Total >= out.Free {
			out.Used = out.Total - out.Free
		}
		if out.Total > 0 {
			out.UsagePercent = float64(out.Used) / float64(out.Total) * 100
		}
		return out, out.Total > 0
	}

	for _, msg := range resp.GetMessages() {
		for _, st := range msg.GetStats() {
			if st.GetMountedOn() == "/var" {
				if d, ok := pick(st); ok {
					return d, nil
				}
			}
		}
	}
	for _, msg := range resp.GetMessages() {
		for _, st := range msg.GetStats() {
			if st.GetMountedOn() == "/" {
				if d, ok := pick(st); ok {
					return d, nil
				}
			}
		}
	}
	return dm, nil
}

// getNetworkMetrics collects network interface stats from the Talos node.
func (c *Client) getNetworkMetrics() (NetworkMetrics, error) {
	nm := NetworkMetrics{}
	addresses := c.getInterfaceAddresses()

	if r, err := c.client.Read(c.ctx, "proc/net/dev"); err == nil {
		data, _ := io.ReadAll(r)
		r.Close()
		lines := strings.Split(string(data), "\n")
		for i := 1; i < len(lines); i++ {
			f := strings.Fields(strings.TrimSpace(lines[i]))
			if len(f) < 2 {
				continue
			}
			// Skip /proc/net/dev header lines ("Inter|" / " face |") which
			// do not have a trailing colon on the interface token.
			if !strings.HasSuffix(f[0], ":") {
				continue
			}
			ifname := strings.TrimSuffix(f[0], ":")
			if ifname == "lo" {
				continue
			}
			nm.Interfaces = append(nm.Interfaces, NetworkInterface{Name: ifname, IPAddress: addresses[ifname]})
			// proc/net/dev layout: iface: rbytes rpackets rerrs rdrop rfifo
			// rframe rcompressed rmulticast | tbytes tpackets ...
			if v, err := parseFloat(f[1]); err == nil {
				nm.BytesRecv += uint64(v)
			}
			if len(f) > 2 {
				if v, err := parseFloat(f[2]); err == nil {
					nm.PacketsRecv += uint64(v)
				}
			}
			if len(f) > 9 {
				if v, err := parseFloat(f[9]); err == nil {
					nm.BytesSent += uint64(v)
				}
			}
			if len(f) > 10 {
				if v, err := parseFloat(f[10]); err == nil {
					nm.PacketsSent += uint64(v)
				}
			}
		}
	}
	return nm, nil
}

func parseFloat(s string) (float64, error) {
	var f float64
	_, err := fmt.Sscanf(s, "%f", &f)
	return f, err
}

// Context returns the client's context.
func (c *Client) Context() context.Context {
	return c.ctx
}

// Client returns the underlying Talos client.
func (c *Client) Client() *client.Client {
	return c.client
}
