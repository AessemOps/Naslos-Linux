package talos

import (
	"fmt"
	"strings"

	"github.com/siderolabs/talos/pkg/machinery/api/storage"
	"github.com/siderolabs/talos/pkg/machinery/config/types/block"
	resblock "github.com/siderolabs/talos/pkg/machinery/resources/block"
)

// VolumeAdvisor provides best-practice recommendations for disk configuration.
type VolumeAdvisor struct{}

// Recommendation is a disk configuration recommendation.
type Recommendation struct {
	// Topology is the recommended topology: "mirror", "raidz1", "raidz2", "raidz3".
	// JSON tags are lowercase to match the UI contract (DiskWizard.svelte
	// reads recommendation.topology/.disks/.description); without them Go's
	// encoding/json emits capitalized keys ("Topology", "Disks", ...) which
	// the UI reads as undefined.
	Topology string `json:"topology"`

	// Disks is the list of disk device paths recommended.
	Disks []string `json:"disks"`

	// Description explains the recommendation.
	Description string `json:"description"`

	// HumanReadableSize is the usable capacity.
	HumanReadableSize string `json:"humanReadableSize"`
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

// GetDiscoveredVolumes fetches discovered disks from the Talos node.
// This returns raw data that can be converted to DiskInfo.
func (c *Client) GetDiscoveredVolumes() ([]*storage.Disk, error) {
	resp, err := c.client.Disks(c.ctx)
	if err != nil {
		return nil, fmt.Errorf("fetching disks: %w", err)
	}
	return resp.Messages[0].Disks, nil
}

// ZFSBestPractices returns the recommended ZFS pool options for Talos.
func ZFSBestPractices() map[string]string {
	return map[string]string{
		"ashift":               "12", // 4K sector alignment
		"mountpoint":           "/var/mnt/<pool>",
		"xattr":                "sa",
		"compression":          "zstd",
		"acltype":              "posixacl",
		"atime":                "off",
		"dnodesize":            "auto",
		"relatime":             "on",
		"recordsize":           "128K",
		"special_small_blocks": "0",
	}
}

// UserVolumeConfig creates a UserVolumeConfig document for ext4/xfs/btrfs volumes.
// ZFS pools are handled separately via the agent, not UserVolumeConfig.
func UserVolumeConfig(name, fsType string, minSize string) (*block.UserVolumeConfigV1Alpha1, error) {
	cfg := block.NewUserVolumeConfigV1Alpha1()
	cfg.MetaName = name

	fst, err := parseFilesystemType(fsType)
	if err != nil {
		return nil, err
	}
	cfg.FilesystemSpec.FilesystemType = fst

	if minSize != "" {
		var size block.ByteSize
		if err := size.UnmarshalText([]byte(minSize)); err != nil {
			return nil, fmt.Errorf("parsing size: %w", err)
		}
		cfg.ProvisioningSpec.ProvisioningMinSize = size
	}
	return cfg, nil
}

// parseFilesystemType maps a filesystem type string to the Talos enum.
func parseFilesystemType(s string) (block.FilesystemType, error) {
	switch strings.ToLower(strings.TrimSpace(s)) {
	case "ext4":
		return resblock.FilesystemTypeEXT4, nil
	case "xfs":
		return resblock.FilesystemTypeXFS, nil
	case "btrfs":
		return resblock.FilesystemTypeBtrfs, nil
	case "vfat":
		return resblock.FilesystemTypeVFAT, nil
	default:
		return resblock.FilesystemTypeNone, fmt.Errorf("unsupported filesystem type %q (supported: ext4, xfs, btrfs, vfat)", s)
	}
}
