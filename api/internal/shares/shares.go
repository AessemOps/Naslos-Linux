// Package shares provides SMB/NFS/Time Machine share management for Naslos.
package shares

import (
	"os"
	"time"
)

// Protocol is the share protocol type.
type Protocol string

const (
	// ProtocolSMB is SMB/CIFS (Windows, macOS, Linux).
	ProtocolSMB Protocol = "smb"
	// ProtocolNFS is Unix (Linux, macOS).
	ProtocolNFS Protocol = "nfs"
	// ProtocolAFP is Time Machine (macOS).
	ProtocolAFP Protocol = "afp"
)

// Share represents a single share configuration.
type Share struct {
	Name         string   `json:"name"`
	Path         string   `json:"path"`
	Protocol     Protocol `json:"protocol"`
	Description  string   `json:"description"`
	ReadOnly     bool     `json:"readOnly"`
	Browseable   bool     `json:"browseable"`
	AllowedHosts []string `json:"allowedHosts"`
	ValidUsers   []string `json:"validUsers"`
	// ValidGroups lists LDAP groups allowed to use the share, rendered into
	// smb.conf as `@group` entries. Kept separate from ValidUsers so a name is
	// never ambiguous between a user and a group.
	ValidGroups []string `json:"validGroups"`
	TimeMachine bool     `json:"timeMachine"`
	// NoRootSquash disables NFS root squashing for this share. Off by default:
	// a root client is mapped to nobody unless an operator opts out, and the UI
	// warns when they do (PF-H4).
	NoRootSquash bool      `json:"noRootSquash,omitempty"`
	CreatedAt    time.Time `json:"createdAt"`
	Enabled      bool      `json:"enabled"`
}

// CreateShareRequest is the request to create a new share.
// Browseable and Enabled are pointers so "omitted" is distinguishable from
// "explicitly false": omitted browseable defaults to true (the historical
// behaviour), omitted enabled defaults to true.
type CreateShareRequest struct {
	Name         string   `json:"name"`
	Path         string   `json:"path"`
	Protocol     Protocol `json:"protocol"`
	Description  string   `json:"description"`
	ReadOnly     bool     `json:"readOnly"`
	Browseable   *bool    `json:"browseable,omitempty"`
	AllowedHosts []string `json:"allowedHosts"`
	ValidUsers   []string `json:"validUsers"`
	ValidGroups  []string `json:"validGroups"`
	TimeMachine  bool     `json:"timeMachine"`
	// NoRootSquash opts this share out of NFS root squashing; the UI warns
	// because it lets a root client write as root (PF-H4).
	NoRootSquash bool  `json:"noRootSquash,omitempty"`
	Enabled      *bool `json:"enabled,omitempty"`
}

// UpdateShareRequest is the request to update a share. Pointer fields are
// optional; a non-nil Description allows clearing the description (an empty
// string is a valid value), which a plain string could not express.
type UpdateShareRequest struct {
	// Path repoints the share at another folder (a subfolder of a dataset is
	// allowed, see FR-SHR-01). The API validates it like a create.
	Path         *string  `json:"path,omitempty"`
	Description  *string  `json:"description,omitempty"`
	ReadOnly     *bool    `json:"readOnly,omitempty"`
	Browseable   *bool    `json:"browseable,omitempty"`
	AllowedHosts []string `json:"allowedHosts,omitempty"`
	ValidUsers   []string `json:"validUsers,omitempty"`
	ValidGroups  []string `json:"validGroups,omitempty"`
	TimeMachine  *bool    `json:"timeMachine,omitempty"`
	NoRootSquash *bool    `json:"noRootSquash,omitempty"`
	Enabled      *bool    `json:"enabled,omitempty"`
}

// Manager manages shares on a ZFS-backed Naslos system.
type Manager struct {
	shares     map[string]*Share
	configPath string
	zfsBase    string
}

// NewManager creates a new share manager. configPath is the JSON file the
// share definitions are persisted to; an empty string keeps the manager
// in-memory only (used by tests). zfsBase is the only directory tree shares
// may point at.
func NewManager(configPath string) *Manager {
	return NewManagerWithBase(configPath, DefaultZFSBase())
}

// NewManagerWithBase creates a share manager with an explicit ZFS base path.
func NewManagerWithBase(configPath, zfsBase string) *Manager {
	if zfsBase == "" {
		zfsBase = DefaultZFSBase()
	}
	m := &Manager{
		shares:     make(map[string]*Share),
		configPath: configPath,
		zfsBase:    zfsBase,
	}
	m.load()
	return m
}

// DefaultZFSBase returns the directory tree shares live under. Overridable
// via SHARES_ZFS_BASE for development outside the Talos host layout.
func DefaultZFSBase() string {
	if v := os.Getenv("SHARES_ZFS_BASE"); v != "" {
		return v
	}
	return "/var/mnt"
}

// ZFSBase returns the configured base path for share datasets.
func (m *Manager) ZFSBase() string {
	return m.zfsBase
}
