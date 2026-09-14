package zfs

import (
	"fmt"
	"os"
	"regexp"
	"sort"
	"strings"
)

// poolNamePattern is the ZFS-safe pool charset: must start alphanumerically.
var poolNamePattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._:-]*$`)

// ValidatePoolName rejects a pool name that ZFS would refuse or that could be
// confused with a path.
func ValidatePoolName(name string) error {
	if strings.TrimSpace(name) == "" {
		return fmt.Errorf("pool name is required")
	}
	if !poolNamePattern.MatchString(name) {
		return fmt.Errorf("invalid pool name %q: must start with a letter or digit and contain only letters, digits, '.', '_', ':' or '-'", name)
	}
	return nil
}

// vdevTopologies are the vdev layouts `zpool add` may create. "" and "single"
// mean one redundancy-less vdev per disk (a stripe); "stripe" is accepted as
// another spelling of that.
var vdevTopologies = map[string]bool{
	"": true, "single": true, "stripe": true,
	"mirror": true, "raidz": true, "raidz1": true, "raidz2": true, "raidz3": true,
}

// normalizeTopology maps the spellings of a redundancy-less vdev to "single".
func normalizeTopology(topology string) string {
	if topology == "stripe" || topology == "" {
		return "single"
	}
	return topology
}

// NormalizeVDevTopology lowercases/trims a topology and rejects unknown ones.
func NormalizeVDevTopology(topology string) (string, error) {
	norm := strings.ToLower(strings.TrimSpace(topology))
	if !vdevTopologies[norm] {
		return "", fmt.Errorf("unsupported topology %q (supported: single, mirror, raidz1, raidz2, raidz3)", topology)
	}
	// "" and "stripe" both mean one vdev per disk. Normalizing here keeps an
	// empty argument out of the `zpool add` command line, where it would be read
	// as a device name.
	return normalizeTopology(norm), nil
}

// vdevMinimumDisks is the number of disks a topology needs. These are ZFS's own
// minimums; giving fewer is rejected by `zpool add` with a less obvious error.
func vdevMinimumDisks(topology string) int {
	switch topology {
	case "mirror", "raidz", "raidz1":
		return 2
	case "raidz2":
		return 3
	case "raidz3":
		return 4
	default: // single
		return 1
	}
}

// vdevLabel names a topology in user-facing messages.
func vdevLabel(topology string) string {
	switch topology {
	case "mirror":
		return "a mirror"
	case "raidz", "raidz1":
		return "raidz1"
	case "raidz2":
		return "raidz2"
	case "raidz3":
		return "raidz3"
	default:
		return "a single-disk vdev"
	}
}

// normalizeDiskPath requires an absolute /dev path and confirms the device
// exists on the node (checked through the /host mount).
func normalizeDiskPath(disk string) (string, error) {
	dev := strings.TrimSpace(disk)
	if dev == "" {
		return "", fmt.Errorf("disk path is required")
	}
	if !strings.HasPrefix(dev, "/dev/") {
		return "", fmt.Errorf("disk %q must be an absolute path under /dev", disk)
	}
	if strings.Contains(dev, "..") || strings.ContainsAny(dev, "\n\r\t") {
		return "", fmt.Errorf("invalid disk path %q", disk)
	}
	if _, err := os.Stat(hostRoot + dev); err != nil {
		return "", fmt.Errorf("disk %s not found on the node", dev)
	}
	return dev, nil
}

// PoolMembers maps every disk currently in a pool to that pool's name, so a disk
// cannot be attached to a second pool (or offered as a fresh one).
func (c *Client) PoolMembers() (map[string]string, error) {
	pools, err := c.Pools()
	if err != nil {
		return nil, err
	}

	members := make(map[string]string)
	for _, pool := range pools {
		for _, disk := range pool.Disks {
			members[disk] = pool.Name
		}
	}
	return members, nil
}

// FreeDisks returns the whole block devices on the node that are not part of any
// pool, sorted - the candidates for `zpool add`.
func (c *Client) FreeDisks() ([]string, error) {
	members, err := c.PoolMembers()
	if err != nil {
		return nil, err
	}

	entries, err := os.ReadDir(hostRoot + "/dev")
	if err != nil {
		return nil, fmt.Errorf("reading /dev: %w", err)
	}

	var free []string
	for _, entry := range entries {
		name := entry.Name()
		if !isWholeDisk(name) {
			continue
		}
		dev := "/dev/" + name
		if _, inUse := members[dev]; inUse {
			continue
		}
		info, err := entry.Info()
		if err != nil || !info.Mode().IsRegular() {
			continue
		}
		free = append(free, dev)
	}
	sort.Strings(free)
	return free, nil
}

// isWholeDisk reports whether a /dev entry name looks like an entire disk rather
// than a partition or an unrelated device.
func isWholeDisk(name string) bool {
	switch {
	case strings.HasPrefix(name, "nvme"):
		// nvme0n1 is a disk; nvme0n1p1 is a partition.
		return !strings.Contains(name, "p")
	case strings.HasPrefix(name, "sd"), strings.HasPrefix(name, "vd"), strings.HasPrefix(name, "hd"):
		// sda/vdb are disks; sda1/vdb12 are partitions.
		return !strings.ContainsAny(name[1:], "0123456789")
	default:
		return false
	}
}

// AddVDev attaches one vdev (a group of disks in a topology) to an existing
// pool, e.g. `zpool add tank mirror /dev/sdb /dev/sdc`.
//
// Three properties of `zpool add` shape the guardrails here:
//   - it does not rebalance: existing data stays where it is, so this adds
//     capacity rather than throughput for data already written;
//   - losing any vdev loses the whole pool, so a redundancy-less vdev added
//     here lowers the pool's fault tolerance;
//   - it writes to the disks, so a disk that belongs to another pool must never
//     be accepted - that would overwrite that pool's label.
func (c *Client) AddVDev(pool, topology string, disks []string, force bool) error {
	if err := ValidatePoolName(pool); err != nil {
		return err
	}
	norm, err := NormalizeVDevTopology(topology)
	if err != nil {
		return err
	}
	if len(disks) == 0 {
		return fmt.Errorf("at least one disk is required")
	}
	if min := vdevMinimumDisks(norm); len(disks) < min {
		return fmt.Errorf("adding %s needs at least %d disks, got %d", vdevLabel(norm), min, len(disks))
	}

	// The pool has to exist first, otherwise `zpool add` reports a confusing
	// "no such pool" after we have already validated the disks.
	if _, err := c.hostExec(zpoolBin, "list", pool); err != nil {
		return fmt.Errorf("pool %q not found", pool)
	}

	// A disk that is already a pool member is either refused by ZFS or, with
	// force, has its old label overwritten - destroying that pool.
	members, err := c.PoolMembers()
	if err != nil {
		return err
	}

	clean := make([]string, 0, len(disks))
	seen := make(map[string]bool, len(disks))
	for _, disk := range disks {
		dev, err := normalizeDiskPath(disk)
		if err != nil {
			return err
		}
		if seen[dev] {
			return fmt.Errorf("disk %s was given more than once", dev)
		}
		seen[dev] = true
		if owner, inUse := members[dev]; inUse {
			return fmt.Errorf("disk %s already belongs to pool %q", dev, owner)
		}
		clean = append(clean, dev)
	}

	args := []string{"add"}
	if force {
		// -f overwrites an unrecognised signature on the disks. Opt-in,
		// because whatever was there is not recoverable.
		args = append(args, "-f")
	}
	args = append(args, pool)
	if norm != "single" {
		args = append(args, norm)
	}
	args = append(args, clean...)

	out, err := c.hostExec(zpoolBin, args...)
	if err != nil {
		return fmt.Errorf("adding %s to pool %s: %s: %w", vdevLabel(norm), pool, strings.TrimSpace(out), err)
	}
	return nil
}
