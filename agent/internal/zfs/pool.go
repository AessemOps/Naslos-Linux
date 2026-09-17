// Package zfs provides ZFS pool and dataset operations for the naslos-agent.
// All operations execute via chroot /host to manage pools on the Talos host.
package zfs

import (
	"context"
	"os"
	"os/exec"
)

const (
	zpoolBin  = "/usr/local/sbin/zpool"
	zfsBin    = "/usr/local/sbin/zfs"
	wipefsBin = "/usr/bin/wipefs"
)

// hostRoot is where the Talos host filesystem is mounted into the agent pod.
// A variable rather than a constant so tests can stand in a temporary tree
// (device paths are validated by stat-ing them through this root).
var hostRoot = "/host"

// hostBinExists checks if a binary exists inside the host root.
// Used to gate optional steps (e.g. wipefs) that Talos may not ship.
func hostBinExists(bin string) bool {
	_, err := os.Stat(hostRoot + bin)
	return err == nil
}

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
	// Mounted is the dataset's mount state in *this* process's mount namespace.
	// The agent runs in the host's, which is what `zfs send` and the on-disk
	// data live in: a dataset created from inside a pod can be mounted in that
	// pod's namespace only, in which case a send from here captures an empty
	// dataset while the data sits on the parent. Callers refuse to back such a
	// dataset up (see the instance's send path).
	Mounted bool `json:"mounted"`
}

// PoolConfig is the configuration for creating a new pool.
type PoolConfig struct {
	Name     string            `json:"name"`
	Topology string            `json:"topology"`
	Disks    []string          `json:"disks"`
	Cache    string            `json:"cache"` // optional cache (L2ARC) device
	Options  map[string]string `json:"options"`
}

// PoolHealth is a structured parse of `zpool status` for the health page.
type PoolHealth struct {
	Name    string       `json:"name"`
	State   string       `json:"state"`
	Scan    string       `json:"scan"`
	Errors  string       `json:"errors"`
	Config  []PoolDevice `json:"config"`
	IOStats PoolIOStats  `json:"ioStats"`
}

// PoolDevice is a single device in the pool config tree.
type PoolDevice struct {
	Name    string       `json:"name"`
	State   string       `json:"state"`
	Read    string       `json:"read"`
	Write   string       `json:"write"`
	Cksum   string       `json:"cksum"`
	Devices []PoolDevice `json:"devices,omitempty"`
}

// PoolIOStats holds `zpool iostat` counters for a pool.
type PoolIOStats struct {
	ReadOps  string `json:"readOps"`
	WriteOps string `json:"writeOps"`
	ReadBW   string `json:"readBW"`
	WriteBW  string `json:"writeBW"`
}

// DefaultOptions returns ZFS best-practice dataset options for Talos.
// mountpoint is applied separately (not via zpool create -o, which
// OpenZFS 2.4.x rejects) and is not included here.
func DefaultOptions() map[string]string {
	return map[string]string{
		"xattr":       "sa",
		"compression": "zstd",
		"acltype":     "posixacl",
		"atime":       "off",
		"dnodesize":   "auto",
		"relatime":    "on",
		"recordsize":  "128K",
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
	return runHost(c.ctx, name, args...)
}

// runHost executes a command in the host's chroot.
//
// It is a variable rather than a plain function so tests can capture the exact
// command line without a host: pool and dataset operations are destructive and
// irreversible (a wrong `zpool add` argument attaches the wrong disk), so the
// arguments deserve to be asserted, not just the happy path.
var runHost = func(ctx context.Context, name string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, "chroot", hostRoot, name)
	cmd.Args = append(cmd.Args, args...)
	out, err := cmd.CombinedOutput()
	return string(out), err
}
