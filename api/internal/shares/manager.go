package shares

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"time"
)

// load loads shares from the config file.
func (m *Manager) load() {
	if m.configPath == "" {
		return
	}

	data, err := os.ReadFile(m.configPath)
	if err != nil {
		return
	}

	var shares []*Share
	if err := json.Unmarshal(data, &shares); err != nil {
		return
	}

	for _, s := range shares {
		m.shares[s.Name] = s
	}
}

// save persists shares to the config file.
func (m *Manager) save() error {
	if m.configPath == "" {
		return nil
	}

	shares := make([]*Share, 0, len(m.shares))
	for _, s := range m.shares {
		shares = append(shares, s)
	}

	data, err := json.MarshalIndent(shares, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling shares: %w", err)
	}

	if err := os.WriteFile(m.configPath, data, 0644); err != nil {
		return fmt.Errorf("writing shares config: %w", err)
	}

	return nil
}

// List returns all shares.
func (m *Manager) List() []*Share {
	shares := make([]*Share, 0, len(m.shares))
	for _, s := range m.shares {
		shares = append(shares, s)
	}
	return shares
}

// ListByProtocol returns shares filtered by protocol.
func (m *Manager) ListByProtocol(protocol Protocol) []*Share {
	var shares []*Share
	for _, s := range m.shares {
		if s.Protocol == protocol {
			shares = append(shares, s)
		}
	}
	return shares
}

// Get returns a share by name.
func (m *Manager) Get(name string) (*Share, error) {
	share, ok := m.shares[name]
	if !ok {
		return nil, fmt.Errorf("share %q not found", name)
	}
	return share, nil
}

// Create creates a new share.
func (m *Manager) Create(req CreateShareRequest) (*Share, error) {
	if req.Name == "" {
		return nil, fmt.Errorf("share name is required")
	}
	if req.Path == "" {
		return nil, fmt.Errorf("share path is required")
	}
	if req.Protocol != ProtocolSMB && req.Protocol != ProtocolNFS && req.Protocol != ProtocolAFP {
		return nil, fmt.Errorf("unsupported protocol: %s", req.Protocol)
	}

	if _, exists := m.shares[req.Name]; exists {
		return nil, fmt.Errorf("share %q already exists", req.Name)
	}

	if !strings.HasPrefix(req.Path, m.zfsBase) {
		return nil, fmt.Errorf("share path must be under %s", m.zfsBase)
	}

	share := &Share{
		Name:         req.Name,
		Path:         req.Path,
		Protocol:     req.Protocol,
		Description:  req.Description,
		ReadOnly:     req.ReadOnly,
		Browseable:   true,
		AllowedHosts: req.AllowedHosts,
		ValidUsers:   req.ValidUsers,
		TimeMachine:  req.TimeMachine,
		CreatedAt:    time.Now(),
		Enabled:      true,
	}

	if req.Browseable != false {
		share.Browseable = req.Browseable
	}

	m.shares[share.Name] = share

	if err := m.save(); err != nil {
		delete(m.shares, share.Name)
		return nil, err
	}

	return share, nil
}

// Update updates an existing share.
func (m *Manager) Update(name string, req UpdateShareRequest) (*Share, error) {
	share, err := m.Get(name)
	if err != nil {
		return nil, err
	}

	if req.Description != "" {
		share.Description = req.Description
	}
	if req.ReadOnly != nil {
		share.ReadOnly = *req.ReadOnly
	}
	if req.Browseable != nil {
		share.Browseable = *req.Browseable
	}
	if req.AllowedHosts != nil {
		share.AllowedHosts = req.AllowedHosts
	}
	if req.ValidUsers != nil {
		share.ValidUsers = req.ValidUsers
	}
	if req.TimeMachine != nil {
		share.TimeMachine = *req.TimeMachine
	}
	if req.Enabled != nil {
		share.Enabled = *req.Enabled
	}

	if err := m.save(); err != nil {
		return nil, err
	}

	return share, nil
}

// Delete removes a share.
func (m *Manager) Delete(name string) error {
	if _, err := m.Get(name); err != nil {
		return err
	}

	delete(m.shares, name)
	return m.save()
}

// ValidatePath checks if a path exists and is under the ZFS base.
func (m *Manager) ValidatePath(path string) error {
	if !strings.HasPrefix(path, m.zfsBase) {
		return fmt.Errorf("path must be under %s", m.zfsBase)
	}

	info, err := os.Stat(path)
	if err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("path does not exist: %s", path)
		}
		return fmt.Errorf("stat path: %w", err)
	}

	if !info.IsDir() {
		return fmt.Errorf("path is not a directory: %s", path)
	}

	return nil
}
