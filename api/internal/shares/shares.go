// Package shares provides SMB/NFS/Time Machine share management for Naslos.
package shares

import (
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
	TimeMachine  bool     `json:"timeMachine"`
	CreatedAt    time.Time `json:"createdAt"`
	Enabled      bool     `json:"enabled"`
}

// CreateShareRequest is the request to create a new share.
type CreateShareRequest struct {
	Name         string   `json:"name"`
	Path         string   `json:"path"`
	Protocol     Protocol `json:"protocol"`
	Description  string   `json:"description"`
	ReadOnly     bool     `json:"readOnly"`
	Browseable   bool     `json:"browseable"`
	AllowedHosts []string `json:"allowedHosts"`
	ValidUsers   []string `json:"validUsers"`
	TimeMachine  bool     `json:"timeMachine"`
}

// UpdateShareRequest is the request to update a share.
type UpdateShareRequest struct {
	Description  string   `json:"description,omitempty"`
	ReadOnly     *bool    `json:"readOnly,omitempty"`
	Browseable   *bool    `json:"browseable,omitempty"`
	AllowedHosts []string `json:"allowedHosts,omitempty"`
	ValidUsers   []string `json:"validUsers,omitempty"`
	TimeMachine  *bool    `json:"timeMachine,omitempty"`
	Enabled      *bool    `json:"enabled,omitempty"`
}

// Manager manages shares on a ZFS-backed Naslos system.
type Manager struct {
	shares     map[string]*Share
	configPath string
	zfsBase    string
}

// NewManager creates a new share manager.
func NewManager(configPath string) *Manager {
	m := &Manager{
		shares:     make(map[string]*Share),
		configPath: configPath,
		zfsBase:    "/var/mnt",
	}
	m.load()
	return m
}
