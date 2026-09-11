// Package zfs provides ZFS pool and dataset operations for the naslos-agent.
// All operations execute via chroot /host to manage pools on the Talos host.
package zfs

import (
	"context"
	"os/exec"
)

const (
	hostRoot  = "/host"
	zpoolBin  = "/usr/local/sbin/zpool"
	zfsBin    = "/usr/local/sbin/zfs"
	wipefsBin = "/usr/bin/wipefs"
)

// Pool represents a ZFS pool.
type Pool struct {
	Name       string   `json:"name"`
	Size       string   `json:"size"`
	Alloc      string   `json:"alloc"`
	Free       string   `json:"free"`
	Health     string   `json:"health"`
	Topology   string   `json:"topology"`
	Disks      []string `json:"disks"`
	Mountpoint string   `json:"mountpoint"`
}

// Dataset represents a ZFS dataset.
type Dataset struct {
	Name       string `json:"name"`
	Used       string `json:"used"`
	Avail      string `json:"avail"`
	Refer      string `json:"refer"`
	Mountpoint string `json:"mountpoint"`
}

// PoolConfig is the configuration for creating a new pool.
type PoolConfig struct {
	Name     string            `json:"name"`
	Topology string            `json:"topology"`
	Disks    []string          `json:"disks"`
	Options  map[string]string `json:"options"`
}

// DefaultOptions returns ZFS best-practice options for Talos.
func DefaultOptions() map[string]string {
	return map[string]string{
		"ashift":               "12",
		"mountpoint":           "/var/mnt/<pool>",
		"xattr":                "sa",
		"compression":          "zstd",
		"acltype":              "posixacl",
		"atime":                "off",
		"dnodesize":            "auto",
		"relatime":             "on",
		"recordsize":           "128K",
		"special_small_blocks": "0",
		"com.sun:auto-snapshot": "false",
	}
}

// Client provides ZFS operations.
type Client struct {
	ctx context.Context
}

// NewClient creates a new ZFS client.
func NewClient(ctx context.Context) *Client {
	return &Client{ctx: ctx}
}

// hostExec runs a command inside the host namespace via chroot.
func (c *Client) hostExec(name string, args ...string) (string, error) {
	cmd := exec.CommandContext(c.ctx, "chroot", hostRoot, name)
	cmd.Args = append(cmd.Args, args...)
	out, err := cmd.CombinedOutput()
	return string(out), err
}
