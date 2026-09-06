package talos

import (
	"fmt"
	"strings"

	"github.com/siderolabs/talos/pkg/machinery/api/machine"
	"github.com/siderolabs/talos/pkg/machinery/config/types/block"
)

// VolumeAdvisor provides best-practice recommendations for disk configuration.
type VolumeAdvisor struct{}

// Recommendation is a disk configuration recommendation.
type Recommendation struct {
	// Topology is the recommended topology: "mirror", "raidz1", "raidz2", "raidz3".
	Topology string

	// Disks is the list of disk device paths recommended.
	Disks []string

	// Description explains the recommendation.
	Description string

	// HumanReadableSize is the usable capacity.
	HumanReadableSize string
}

// NewVolumeAdvisor creates a new VolumeAdvisor.
func NewVolumeAdvisor() *VolumeAdvisor {
	return &VolumeAdvisor{}
}

// Recommend returns a volume recommendation for the given disks.
func (va *VolumeAdvisor) Recommend(disks []DiskInfo) (*Recommendation, error) {
	n := len(disks)
	if n == 0 {
		return nil, fmt.Errorf("no disks provided")
	}

	// Filter out system disks
	var usable []DiskInfo
	for _, d := range disks {
		if !d.IsSystemDisk {
			usable = append(usable, d)
		}
	}
	n = len(usable)

	switch {
	case n == 1:
		return &Recommendation{
			Topology:    "single",
			Disks:       devicePaths(usable),
			Description: "Single disk — no redundancy. Use only for non-critical data or cache.",
		}, nil

	case n == 2:
		return &Recommendation{
			Topology:    "mirror",
			Disks:       devicePaths(usable),
			Description: "Mirror (RAID1) — maximum redundancy for 2 disks. Usable capacity = size of smallest disk.",
		}, nil

	case n >= 3 && n <= 5:
		return &Recommendation{
			Topology:    "raidz1",
			Disks:       devicePaths(usable),
			Description: "RAIDZ1 (single parity) — good balance of capacity and redundancy for 3-5 disks.",
		}, nil

	case n >= 6 && n <= 10:
		return &Recommendation{
			Topology:    "raidz2",
			Disks:       devicePaths(usable),
			Description: "RAIDZ2 (double parity) — recommended for 6-10 disks. Survives any 2 disk failures.",
		}, nil

	default:
		return &Recommendation{
			Topology:    "raidz3",
			Disks:       devicePaths(usable),
			Description: "RAIDZ3 (triple parity) — recommended for 11+ disks. Survives any 3 disk failures.",
		}, nil
	}
}

// DiskInfo describes a discovered disk.
type DiskInfo struct {
	DevicePath   string
	Size         uint64
	IsSystemDisk bool
	Model        string
	Serial       string
	IsSSD        bool
}

func devicePaths(disks []DiskInfo) []string {
	paths := make([]string, len(disks))
	for i, d := range disks {
		paths[i] = d.DevicePath
	}
	return paths
}

// GetDiscoveredVolumes fetches discovered volumes from the Talos node.
// This returns raw data that can be converted to DiskInfo.
func (c *Client) GetDiscoveredVolumes() ([]*machine.Disk, error) {
	resp, err := c.client.Disks(c.ctx)
	if err != nil {
		return nil, fmt.Errorf("fetching disks: %w", err)
	}
	return resp.Messages[0].Disks, nil
}

// ZFSBestPractices returns the recommended ZFS pool options for Talos.
func ZFSBestPractices() map[string]string {
	return map[string]string{
		"ashift":            "12", // 4K sector alignment
		"mountpoint":        "/var/mnt/<pool>",
		"xattr":             "sa",
		"compression":       "zstd",
		"acltype":           "posixacl",
		"atime":             "off",
		"dnodesize":         "auto",
		"relatime":          "on",
		"recordsize":        "128K",
		"special_small_blocks": "0",
	}
}

// UserVolumeConfig creates a UserVolumeConfig document for ext4/xfs/btrfs volumes.
// ZFS pools are handled separately via the agent, not UserVolumeConfig.
func UserVolumeConfig(name, fsType string, minSize string) (*block.UserVolumeConfigV1Alpha1, error) {
	cfg := block.NewUserVolumeConfigV1Alpha1()
	cfg.MetaName = name
	cfg.FilesystemSpec.FilesystemType = block.FilesystemType(fsType)

	if minSize != "" {
		size, err := parseSize(minSize)
		if err != nil {
			return nil, fmt.Errorf("parsing size: %w", err)
		}
		cfg.ProvisioningSpec.ProvisioningMinSize = uint64(size)
	}
	return cfg, nil
}

// parseSize parses a human-readable size string (e.g. "100GB", "2TB").
func parseSize(s string) (uint64, error) {
	s = strings.TrimSpace(s)
	if s == "" {
		return 0, nil
	}
	// Simple parsing — in production, use resource.Quantity
	var value float64
	var unit string
	if _, err := fmt.Sscanf(s, "%f%s", &value, &unit); err != nil {
		return 0, fmt.Errorf("invalid size %q: %w", s, err)
	}
	switch strings.ToUpper(unit) {
	case "B", "":
		return uint64(value), nil
	case "KB", "KIB":
		return uint64(value * 1024), nil
	case "MB", "MIB":
		return uint64(value * 1024 * 1024), nil
	case "GB", "GIB":
		return uint64(value * 1024 * 1024 * 1024), nil
	case "TB", "TIB":
		return uint64(value * 1024 * 1024 * 1024 * 1024), nil
	}
	return 0, fmt.Errorf("unknown unit %q", unit)
}
