package zfs

import (
	"fmt"
	"strings"
)

// Pools lists all ZFS pools with their member disks.
func (c *Client) Pools() ([]Pool, error) {
	out, err := c.hostExec(zpoolBin, "list", "-H", "-o", "name,size,alloc,free,health")
	if err != nil {
		return nil, fmt.Errorf("listing pools: %w", err)
	}

	var pools []Pool
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line == "" {
			continue
		}
		fields := strings.Split(line, "\t")
		if len(fields) < 5 {
			continue
		}
		pool := Pool{
			Name:   fields[0],
			Size:   fields[1],
			Alloc:  fields[2],
			Free:   fields[3],
			Health: fields[4],
		}
		// Populate member disks by parsing `zpool status`. This lets the
		// disk-setup wizard mark disks that are already part of a pool so
		// the user cannot accidentally add them to another one.
		if disks, err := c.poolDisks(pool.Name); err == nil {
			pool.Disks = disks
		}
		pools = append(pools, pool)
	}
	return pools, nil
}

// poolDisks returns the member devices of a pool by parsing `zpool status`.
// It walks the config section and collects leaf devices (real disks),
// skipping virtual devices (mirror-*, raidz*, cache, spare, logs).
func (c *Client) poolDisks(name string) ([]string, error) {
	if err := ValidatePoolName(name); err != nil {
		return nil, err
	}
	out, err := c.hostExec(zpoolBin, "status", name)
	if err != nil {
		return nil, err
	}

	var disks []string
	inConfig := false
	headerSeen := false
	for _, line := range strings.Split(out, "\n") {
		trimmed := strings.TrimSpace(line)
		if trimmed == "config:" {
			inConfig = true
			continue
		}
		if !inConfig {
			continue
		}
		// Skip empty lines and the header line (NAME STATE READ WRITE CKSUM).
		if trimmed == "" {
			continue
		}
		if !headerSeen {
			headerSeen = true
			continue
		}
		fields := strings.Fields(trimmed)
		if len(fields) < 2 {
			continue
		}
		dev := fields[0]
		// Skip the pool name line (same indentation as header) and virtual
		// devices (mirror-*, raidz*, cache-*, spare-*, logs). Real disks are
		// indented further and have simple names (vdb, sda, nvme0n1, ...).
		if dev == name || strings.Contains(dev, "-") || strings.Contains(dev, ":") {
			continue
		}
		// Normalize to /dev/<name> if not already a full path.
		if !strings.HasPrefix(dev, "/dev/") {
			dev = "/dev/" + dev
		}
		disks = append(disks, dev)
	}
	return disks, nil
}

// PoolStatus returns detailed status of a pool.
func (c *Client) PoolStatus(name string) (string, error) {
	if err := ValidatePoolName(name); err != nil {
		return "", err
	}
	out, err := c.hostExec(zpoolBin, "status", name)
	if err != nil {
		return "", fmt.Errorf("pool status: %w", err)
	}
	return out, nil
}

// CreatePool creates a new ZFS pool with best-practice options.
//
// Every input reaches `zpool create`/`zfs set` argv or wipes a device, so each
// one is validated first (NAS-003): a valid pool name, a known topology, disks
// that exist and belong to no other pool, and only allow-listed dataset options.
func (c *Client) CreatePool(cfg PoolConfig) error {
	if err := ValidatePoolName(cfg.Name); err != nil {
		return err
	}
	topology, err := NormalizeVDevTopology(cfg.Topology)
	if err != nil {
		return err
	}
	if err := ValidateDatasetOptions(cfg.Options); err != nil {
		return err
	}
	if len(cfg.Disks) < 1 {
		return invalidf("at least one disk is required")
	}
	if min := vdevMinimumDisks(topology); len(cfg.Disks) < min {
		return invalidf("creating %s needs at least %d disks, got %d", vdevLabel(topology), min, len(cfg.Disks))
	}

	// Check if pool already exists
	if _, err := c.hostExec(zpoolBin, "list", cfg.Name); err == nil {
		return invalidf("pool %q already exists", cfg.Name)
	}

	// A disk that belongs to another pool must never be accepted: `zpool create
	// -f` would overwrite its label and destroy that pool.
	members, err := c.PoolMembers()
	if err != nil {
		return err
	}
	disks, err := normalizeDiskSet(cfg.Disks, members)
	if err != nil {
		return err
	}
	cache := ""
	if cfg.Cache != "" {
		normalized, err := normalizeDiskSet([]string{cfg.Cache}, members)
		if err != nil {
			return err
		}
		cache = normalized[0]
		for _, disk := range disks {
			if disk == cache {
				return invalidf("disk %s cannot be both a data disk and the cache device", cache)
			}
		}
	}

	// Wipe disks to remove any existing filesystem signatures.
	// Talos' ZFS extension ships only zpool/zfs (no wipefs), so skip
	// gracefully when the binary is absent — `zpool create -f` handles
	// fresh disks (e.g. vdb/vdc) on its own.
	if hostBinExists(wipefsBin) {
		for _, diskPath := range disks {
			if _, err := c.hostExec(wipefsBin, "--all", diskPath); err != nil {
				return fmt.Errorf("wiping %s: %w", diskPath, err)
			}
		}
	}

	// Build zpool create command with pool-level options only.
	// OpenZFS 2.4.x rejects dataset properties (mountpoint, compression,
	// xattr, etc.) in `zpool create -o`; those are set afterwards via
	// `zfs set`. Talos also has a read-only root FS, so the pool's default
	// mount would fail — we set a writable mountpoint after creation.
	args := []string{"create", "-f", "-o", "ashift=12"}
	args = append(args, cfg.Name)

	// Build topology
	switch topology {
	case "mirror":
		args = append(args, "mirror")
		args = append(args, disks...)
	case "raidz1", "raidz", "raidz2", "raidz3":
		args = append(args, topology)
		args = append(args, disks...)
	default: // single
		args = append(args, disks...)
	}

	out, err := c.hostExec(zpoolBin, args...)
	// `zpool create` may "fail" with a mount error on Talos (read-only root
	// FS) even though the pool was created successfully. Verify the pool
	// exists rather than trusting the exit code.
	if err != nil {
		if _, lerr := c.hostExec(zpoolBin, "list", cfg.Name); lerr != nil {
			return fmt.Errorf("creating pool: %s: %w", out, err)
		}
		// Pool exists despite the error (likely a mount issue) — continue.
	}

	// Set mountpoint to a writable path (Talos root is read-only).
	mp := "/var/mnt/" + cfg.Name
	if _, err := c.hostExec(zfsBin, "set", "mountpoint="+mp, cfg.Name); err != nil {
		return fmt.Errorf("setting mountpoint: %w", err)
	}

	// Apply dataset-level options (compression, xattr, acltype, etc.)
	opts := DefaultOptions()
	for k, v := range cfg.Options {
		opts[k] = v
	}
	for k, v := range opts {
		// mountpoint already applied above; skip to avoid duplicate.
		if k == "mountpoint" {
			continue
		}
		if _, err := c.hostExec(zfsBin, "set", k+"="+v, cfg.Name); err != nil {
			return fmt.Errorf("setting %s: %w", k, err)
		}
	}

	// Disable SELinux contexts (required for Talos)
	if _, err := c.hostExec(zfsBin, "set",
		"context=none", "fscontext=none", "defcontext=none", "rootcontext=none",
		cfg.Name); err != nil {
		return fmt.Errorf("disabling SELinux on pool: %w", err)
	}

	// Add optional cache (L2ARC) device. Cache devices are added after
	// pool creation via `zpool add` — they cannot be included in the
	// initial `zpool create` command.
	if cache != "" {
		if _, err := c.hostExec(zpoolBin, "add", cfg.Name, "cache", cache); err != nil {
			return fmt.Errorf("adding cache device %s: %w", cache, err)
		}
	}

	return nil
}

// DestroyPool destroys a ZFS pool.
func (c *Client) DestroyPool(name string) error {
	if err := ValidatePoolName(name); err != nil {
		return err
	}
	out, err := c.hostExec(zpoolBin, "destroy", "-f", name)
	if err != nil {
		return fmt.Errorf("destroying pool: %s: %w", out, err)
	}
	return nil
}

// ImportablePool is a pool that exists on disk but is not currently imported.
type ImportablePool struct {
	Name     string   `json:"name"`
	State    string   `json:"state"`
	Topology string   `json:"topology"`
	Disks    []string `json:"disks"`
}

// ListImportable runs `zpool import` (dry-run) and returns pools that can be
// imported but are not currently active.
func (c *Client) ListImportable() ([]ImportablePool, error) {
	out, err := c.hostExec(zpoolBin, "import")
	if err != nil {
		return nil, fmt.Errorf("listing importable pools: %w", err)
	}

	var pools []ImportablePool
	var cur *ImportablePool
	inConfig := false
	headerSeen := false

	for _, line := range strings.Split(out, "\n") {
		trimmed := strings.TrimSpace(line)

		// Each pool block starts with "pool: <name>".
		if strings.HasPrefix(trimmed, "pool:") {
			if cur != nil {
				pools = append(pools, *cur)
			}
			cur = &ImportablePool{
				Name: strings.TrimSpace(strings.TrimPrefix(trimmed, "pool:")),
			}
			inConfig = false
			headerSeen = false
			continue
		}
		if cur == nil {
			continue
		}

		// Parse key: value lines outside the config section.
		if !inConfig {
			if idx := strings.Index(trimmed, ":"); idx > 0 {
				key := strings.TrimSpace(trimmed[:idx])
				val := strings.TrimSpace(trimmed[idx+1:])
				if key == "state" {
					cur.State = val
				}
			}
			if trimmed == "config:" {
				inConfig = true
				headerSeen = false
			}
			continue
		}

		// Inside config section: skip empty lines and header, then parse device tree.
		if trimmed == "" {
			if headerSeen {
				inConfig = false
			}
			continue
		}
		if !headerSeen {
			headerSeen = true
			continue
		}

		fields := strings.Fields(trimmed)
		if len(fields) < 2 {
			continue
		}
		dev := fields[0]
		indent := len(line) - len(strings.TrimLeft(line, " \t"))

		// Top-level vdev: determine topology.
		if indent <= 1 {
			if strings.HasPrefix(dev, "mirror") {
				cur.Topology = "mirror"
			} else if strings.HasPrefix(dev, "raidz3") {
				cur.Topology = "raidz3"
			} else if strings.HasPrefix(dev, "raidz2") {
				cur.Topology = "raidz2"
			} else if strings.HasPrefix(dev, "raidz") {
				cur.Topology = "raidz1"
			} else if dev != cur.Name {
				cur.Topology = "single"
			}
		} else {
			// Leaf device — collect if it looks like a real disk.
			if !strings.Contains(dev, "-") && !strings.HasPrefix(dev, cur.Name) {
				if !strings.HasPrefix(dev, "/dev/") {
					dev = "/dev/" + dev
				}
				cur.Disks = append(cur.Disks, dev)
			}
		}
	}
	if cur != nil {
		pools = append(pools, *cur)
	}
	return pools, nil
}

// ImportPool imports an existing pool. When name is empty, imports all
// available pools; otherwise imports the named pool specifically.
func (c *Client) ImportPool(name string) error {
	var args []string
	if name == "" {
		args = []string{"import", "-f"} // import all
	} else {
		if err := ValidatePoolName(name); err != nil {
			return err
		}
		args = []string{"import", "-f", name}
	}
	out, err := c.hostExec(zpoolBin, args...)
	if err != nil {
		return fmt.Errorf("importing pool: %s: %w", out, err)
	}
	return nil
}

// ExportPool exports a pool.
func (c *Client) ExportPool(name string) error {
	if err := ValidatePoolName(name); err != nil {
		return err
	}
	out, err := c.hostExec(zpoolBin, "export", name)
	if err != nil {
		return fmt.Errorf("exporting pool: %s: %w", out, err)
	}
	return nil
}

// PoolHealth returns structured health data for a pool by parsing
// `zpool status` and `zpool iostat`.
func (c *Client) PoolHealth(name string) (*PoolHealth, error) {
	if err := ValidatePoolName(name); err != nil {
		return nil, err
	}
	out, err := c.hostExec(zpoolBin, "status", name)
	if err != nil {
		return nil, fmt.Errorf("getting pool status: %w", err)
	}

	h := &PoolHealth{Name: name}
	var currentDev *PoolDevice
	inConfig := false
	headerSeen := false

	for _, line := range strings.Split(out, "\n") {
		trimmed := strings.TrimSpace(line)

		// Parse key: value lines outside the config section.
		if !inConfig {
			if idx := strings.Index(trimmed, ":"); idx > 0 {
				key := strings.TrimSpace(trimmed[:idx])
				val := strings.TrimSpace(trimmed[idx+1:])
				switch key {
				case "pool":
					h.Name = val
				case "state":
					h.State = val
				case "scan":
					h.Scan = val
				case "errors":
					h.Errors = val
				}
			}
			if trimmed == "config:" {
				inConfig = true
				headerSeen = false
			}
			continue
		}

		// Inside config section: skip empty lines and header, then parse
		// device tree. The config section ends at the first empty line
		// after devices (before the "errors:" summary).
		if trimmed == "" {
			if headerSeen {
				inConfig = false
			}
			continue
		}
		if !headerSeen {
			headerSeen = true
			continue
		}

		fields := strings.Fields(trimmed)
		if len(fields) < 5 {
			continue
		}
		dev := PoolDevice{
			Name:  fields[0],
			State: fields[1],
			Read:  fields[2],
			Write: fields[3],
			Cksum: fields[4],
		}

		// Indentation determines depth: top-level vdevs have no leading
		// spaces in the first field; children are indented further.
		indent := len(line) - len(strings.TrimLeft(line, " \t"))
		if indent <= 1 {
			// Top-level vdev (mirror-0, raidz, cache, etc. or a single disk).
			h.Config = append(h.Config, dev)
			currentDev = &h.Config[len(h.Config)-1]
		} else if currentDev != nil {
			// Child device — append to the last top-level vdev.
			currentDev.Devices = append(currentDev.Devices, dev)
		}
	}

	// Fetch I/O stats.
	if ioOut, err := c.hostExec(zpoolBin, "iostat", "-v", name, "1", "1"); err == nil {
		h.IOStats = parseIOStats(ioOut, name)
	}

	return h, nil
}

// parseIOStats extracts the pool's I/O counters from `zpool iostat -v`.
func parseIOStats(out, poolName string) PoolIOStats {
	var stats PoolIOStats
	for _, line := range strings.Split(out, "\n") {
		fields := strings.Fields(line)
		// The header is: ... capacity operations bandwidth
		// The pool line has: <pool> <alloc> <cap> <read> <write> <read> <write>
		if len(fields) >= 7 && fields[0] == poolName {
			stats.ReadOps = fields[3]
			stats.WriteOps = fields[4]
			stats.ReadBW = fields[5]
			stats.WriteBW = fields[6]
			break
		}
	}
	return stats
}
