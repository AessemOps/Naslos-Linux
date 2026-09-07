package zfs

import (
	"fmt"
	"strings"
)

// Pools lists all ZFS pools.
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
		pools = append(pools, Pool{
			Name:   fields[0],
			Size:   fields[1],
			Alloc:  fields[2],
			Free:   fields[3],
			Health: fields[4],
		})
	}
	return pools, nil
}

// PoolStatus returns detailed status of a pool.
func (c *Client) PoolStatus(name string) (string, error) {
	out, err := c.hostExec(zpoolBin, "status", name)
	if err != nil {
		return "", fmt.Errorf("pool status: %w", err)
	}
	return out, nil
}

// CreatePool creates a new ZFS pool with best-practice options.
func (c *Client) CreatePool(cfg PoolConfig) error {
	if cfg.Name == "" {
		return fmt.Errorf("pool name is required")
	}
	if len(cfg.Disks) < 1 {
		return fmt.Errorf("at least one disk is required")
	}

	// Check if pool already exists
	_, err := c.hostExec(zpoolBin, "list", cfg.Name)
	if err == nil {
		return fmt.Errorf("pool %q already exists", cfg.Name)
	}

	// Wipe disks to remove any existing filesystem signatures
	for _, disk := range cfg.Disks {
		diskPath := disk
		if !strings.HasPrefix(disk, "/dev/") {
			diskPath = "/dev/" + disk
		}
		if _, err := c.hostExec(wipefsBin, "--all", diskPath); err != nil {
			return fmt.Errorf("wiping %s: %w", diskPath, err)
		}
	}

	// Build pool options
	opts := DefaultOptions()
	for k, v := range cfg.Options {
		opts[k] = v
	}
	// Replace <pool> placeholder in mountpoint
	if mp, ok := opts["mountpoint"]; ok {
		opts["mountpoint"] = strings.Replace(mp, "<pool>", cfg.Name, 1)
	}

	// Build zpool create command
	args := []string{"create", "-f"}
	for k, v := range opts {
		args = append(args, "-o", fmt.Sprintf("%s=%s", k, v))
	}
	// Set aclmode=restricted for better SMB compatibility
	args = append(args, "-O", "aclmode=restricted")
	args = append(args, cfg.Name)

	// Build topology
	switch cfg.Topology {
	case "mirror":
		args = append(args, "mirror")
		args = append(args, cfg.Disks...)
	case "raidz1", "raidz", "raidz2", "raidz3":
		args = append(args, cfg.Topology)
		args = append(args, cfg.Disks...)
	case "single", "":
		args = append(args, cfg.Disks...)
	default:
		return fmt.Errorf("unsupported topology: %s", cfg.Topology)
	}

	out, err := c.hostExec(zpoolBin, args...)
	if err != nil {
		return fmt.Errorf("creating pool: %s: %w", out, err)
	}

	// Disable SELinux contexts (required for Talos)
	if _, err := c.hostExec(zfsBin, "set",
		"context=none", "fscontext=none", "defcontext=none", "rootcontext=none",
		cfg.Name); err != nil {
		return fmt.Errorf("disabling SELinux on pool: %w", err)
	}

	return nil
}

// DestroyPool destroys a ZFS pool.
func (c *Client) DestroyPool(name string) error {
	if name == "" {
		return fmt.Errorf("pool name is required")
	}
	out, err := c.hostExec(zpoolBin, "destroy", "-f", name)
	if err != nil {
		return fmt.Errorf("destroying pool: %s: %w", out, err)
	}
	return nil
}

// ImportPool imports an existing pool.
func (c *Client) ImportPool(name string) error {
	args := []string{"import", "-fal"}
	if name != "" {
		args = append(args, name)
	}
	out, err := c.hostExec(zpoolBin, args...)
	if err != nil {
		return fmt.Errorf("importing pool: %s: %w", out, err)
	}
	return nil
}

// ExportPool exports a pool.
func (c *Client) ExportPool(name string) error {
	out, err := c.hostExec(zpoolBin, "export", name)
	if err != nil {
		return fmt.Errorf("exporting pool: %s: %w", out, err)
	}
	return nil
}
