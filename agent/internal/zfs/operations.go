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

	// Wipe disks to remove any existing filesystem signatures.
	// Talos' ZFS extension ships only zpool/zfs (no wipefs), so skip
	// gracefully when the binary is absent — `zpool create -f` handles
	// fresh disks (e.g. vdb/vdc) on its own.
	if hostBinExists(wipefsBin) {
		for _, disk := range cfg.Disks {
			diskPath := disk
			if !strings.HasPrefix(disk, "/dev/") {
				diskPath = "/dev/" + disk
			}
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
