package shares

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
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

// save persists shares to the config file atomically (temp file + rename) so a
// crash mid-write cannot leave a truncated config behind.
func (m *Manager) save() error {
	if m.configPath == "" {
		return nil
	}

	shares := m.List()

	data, err := json.MarshalIndent(shares, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling shares: %w", err)
	}
	data = append(data, '\n')

	if dir := filepath.Dir(m.configPath); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0755); err != nil {
			return fmt.Errorf("creating shares config directory: %w", err)
		}
	}

	tmp, err := os.CreateTemp(filepath.Dir(m.configPath), ".shares-*.tmp")
	if err != nil {
		return fmt.Errorf("creating temp shares config: %w", err)
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)

	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return fmt.Errorf("writing shares config: %w", err)
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return fmt.Errorf("syncing shares config: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("closing shares config: %w", err)
	}
	if err := os.Chmod(tmpName, 0644); err != nil {
		return fmt.Errorf("setting shares config mode: %w", err)
	}
	if err := os.Rename(tmpName, m.configPath); err != nil {
		return fmt.Errorf("replacing shares config: %w", err)
	}

	return nil
}

// List returns all shares, ordered by name for stable API responses.
func (m *Manager) List() []*Share {
	shares := make([]*Share, 0, len(m.shares))
	for _, s := range m.shares {
		shares = append(shares, s)
	}
	sort.Slice(shares, func(i, j int) bool { return shares[i].Name < shares[j].Name })
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
	if err := validateShareName(req.Name); err != nil {
		return nil, err
	}
	cleanPath, err := m.normalizePath(req.Path)
	if err != nil {
		return nil, err
	}
	if req.Protocol != ProtocolSMB && req.Protocol != ProtocolNFS {
		return nil, fmt.Errorf("unsupported protocol %q: supported protocols are %q and %q",
			req.Protocol, ProtocolSMB, ProtocolNFS)
	}

	if _, exists := m.shares[req.Name]; exists {
		return nil, fmt.Errorf("share %q already exists", req.Name)
	}

	// Omitted fields default to the permissive value: a share is browseable
	// and enabled unless the caller explicitly says otherwise.
	browseable := true
	if req.Browseable != nil {
		browseable = *req.Browseable
	}
	enabled := true
	if req.Enabled != nil {
		enabled = *req.Enabled
	}

	share := &Share{
		Name:         req.Name,
		Path:         cleanPath,
		Protocol:     req.Protocol,
		Description:  req.Description,
		ReadOnly:     req.ReadOnly,
		Browseable:   browseable,
		AllowedHosts: normalizeList(req.AllowedHosts),
		ValidUsers:   normalizeList(req.ValidUsers),
		TimeMachine:  req.TimeMachine,
		CreatedAt:    time.Now().UTC(),
		Enabled:      enabled,
	}

	m.shares[share.Name] = share

	if err := m.save(); err != nil {
		delete(m.shares, share.Name)
		return nil, err
	}

	return share, nil
}

// Update updates an existing share. Only the fields present in req change;
// notably a non-nil Description may clear the description.
func (m *Manager) Update(name string, req UpdateShareRequest) (*Share, error) {
	share, err := m.Get(name)
	if err != nil {
		return nil, err
	}

	previous := *share

	if req.Description != nil {
		share.Description = *req.Description
	}
	if req.ReadOnly != nil {
		share.ReadOnly = *req.ReadOnly
	}
	if req.Browseable != nil {
		share.Browseable = *req.Browseable
	}
	if req.AllowedHosts != nil {
		share.AllowedHosts = normalizeList(req.AllowedHosts)
	}
	if req.ValidUsers != nil {
		share.ValidUsers = normalizeList(req.ValidUsers)
	}
	if req.TimeMachine != nil {
		share.TimeMachine = *req.TimeMachine
	}
	if req.Enabled != nil {
		share.Enabled = *req.Enabled
	}

	if err := m.save(); err != nil {
		*share = previous
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

// ValidatePath checks that path is inside the ZFS base tree and exists as a
// directory. The path is compared after canonicalisation so that traversal
// sequences such as "/var/mnt/../../etc" cannot escape the base.
func (m *Manager) ValidatePath(path string) error {
	clean, err := m.normalizePath(path)
	if err != nil {
		return err
	}

	info, err := os.Stat(clean)
	if err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("path does not exist: %s", clean)
		}
		return fmt.Errorf("stat path: %w", err)
	}

	if !info.IsDir() {
		return fmt.Errorf("path is not a directory: %s", clean)
	}

	return nil
}

// normalizePath canonicalises a share path and rejects anything that is not
// strictly inside the configured ZFS base directory.
func (m *Manager) normalizePath(path string) (string, error) {
	if strings.TrimSpace(path) == "" {
		return "", fmt.Errorf("share path is required")
	}

	clean := filepath.Clean(path)
	base := filepath.Clean(m.zfsBase)
	if clean == base || !strings.HasPrefix(clean, base+string(filepath.Separator)) {
		return "", fmt.Errorf("share path must be a directory inside %s", base)
	}
	return clean, nil
}

// validateShareName rejects names that are empty, contain path separators or
// control characters, or would be ambiguous as a config section / export key.
func validateShareName(name string) error {
	if strings.TrimSpace(name) == "" {
		return fmt.Errorf("share name is required")
	}
	if len(name) > 80 {
		return fmt.Errorf("share name must be 80 characters or fewer")
	}
	if strings.ContainsAny(name, "/\\[]\"':*?<>=+;,") {
		return fmt.Errorf("share name must not contain any of / \\ [ ] \" ' : * ? < > = + ; ,")
	}
	if strings.TrimSpace(name) != name {
		return fmt.Errorf("share name must not start or end with whitespace")
	}
	return nil
}

// normalizeList trims entries, drops empties and removes duplicates while
// preserving order. Always returns a non-nil slice so API responses encode
// [] rather than null.
func normalizeList(in []string) []string {
	out := make([]string, 0, len(in))
	seen := make(map[string]struct{}, len(in))
	for _, v := range in {
		v = strings.TrimSpace(v)
		if v == "" {
			continue
		}
		if _, dup := seen[v]; dup {
			continue
		}
		seen[v] = struct{}{}
		out = append(out, v)
	}
	return out
}
