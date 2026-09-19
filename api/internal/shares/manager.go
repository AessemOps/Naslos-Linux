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
		if s == nil || s.Name == "" {
			continue
		}
		// Normalise the list fields: a share stored before a field existed (or
		// with an empty array) would otherwise be served as null, and clients
		// would have to defend against it.
		s.AllowedHosts = normalizeList(s.AllowedHosts)
		s.ValidUsers = normalizeList(s.ValidUsers)
		s.ValidGroups = normalizeList(s.ValidGroups)
		// The config file is editable by hand: never load a share whose fields
		// would render into directives (NAS-007).
		if err := validateShareFields(s); err != nil {
			continue
		}
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
		if err := os.MkdirAll(dir, 0750); err != nil {
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
	if err := os.Chmod(tmpName, 0600); err != nil {
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
		ValidGroups:  normalizeList(req.ValidGroups),
		TimeMachine:  req.TimeMachine,
		CreatedAt:    time.Now().UTC(),
		Enabled:      enabled,
	}
	if err := validateShareFields(share); err != nil {
		return nil, err
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

	if req.Path != nil {
		cleanPath, err := m.normalizePath(*req.Path)
		if err != nil {
			return nil, err
		}
		share.Path = cleanPath
	}
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
	if req.ValidGroups != nil {
		share.ValidGroups = normalizeList(req.ValidGroups)
	}
	if req.TimeMachine != nil {
		share.TimeMachine = *req.TimeMachine
	}
	if req.Enabled != nil {
		share.Enabled = *req.Enabled
	}

	if err := validateShareFields(share); err != nil {
		*share = previous
		return nil, err
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

// PathOnDataset reports whether path lives on one of the given ZFS dataset
// mountpoints, and returns the dataset it belongs to.
//
// This is the guard that keeps share data on ZFS. A path that is merely inside
// the ZFS base directory - e.g. /var/mnt/tank when "tank" is a plain directory
// rather than a dataset - lives on Talos's EPHEMERAL partition instead: no
// checksums, no snapshots, no redundancy, invisible to pool operations, and
// wiped by a Talos upgrade. Accepting such a share silently puts user data
// outside the pool, so it must be refused.
//
// Mountpoints that cannot contain user data ("none", "-", "/") are ignored; "/"
// in particular would otherwise match every path.
func PathOnDataset(path string, mountpoints []string) (string, bool) {
	clean := filepath.Clean(path)
	if !strings.HasPrefix(clean, "/") {
		return "", false
	}

	best := ""
	for _, mp := range mountpoints {
		mp = filepath.Clean(strings.TrimSpace(mp))
		if mp == "" || mp == "." || mp == "/" || !strings.HasPrefix(mp, "/") {
			continue
		}
		if clean == mp || strings.HasPrefix(clean, mp+string(filepath.Separator)) {
			// Nested datasets exist, so the most specific mountpoint wins.
			if len(mp) > len(best) {
				best = mp
			}
		}
	}
	return best, best != ""
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
	// A newline or tab in the name would start a new smb.conf section (NAS-007).
	if strings.ContainsAny(name, "\n\r\x00\t") {
		return fmt.Errorf("share name must not contain control characters")
	}
	if strings.TrimSpace(name) != name {
		return fmt.Errorf("share name must not start or end with whitespace")
	}
	return nil
}

// validateShareFields rejects the characters that would let a share field start a
// new directive in smb.conf or a new statement/block in ganesha.conf. The
// renderers interpolate these values verbatim, so a newline in a description or a
// semicolon in an export list would otherwise become configuration (NAS-007).
func validateShareFields(share *Share) error {
	if err := validateNoControlChars("description", share.Description); err != nil {
		return err
	}
	if err := validateNoControlChars("path", share.Path); err != nil {
		return err
	}
	for _, group := range []struct {
		kind    string
		entries []string
	}{
		{"allowed host", share.AllowedHosts},
		{"valid user", share.ValidUsers},
		{"valid group", share.ValidGroups},
	} {
		for _, entry := range group.entries {
			if err := validateNoControlChars(group.kind, entry); err != nil {
				return err
			}
			// These lists are rendered space-separated into smb.conf and
			// comma/space separated into ganesha.conf, where `;` ends a statement
			// and `"` opens a string.
			if strings.ContainsAny(entry, ";\"") {
				return fmt.Errorf("%s %q must not contain ';' or '\"'", group.kind, entry)
			}
		}
	}
	return nil
}

// validateNoControlChars rejects newline, carriage return, NUL and tab: every one
// of them can end the current line and let the rest be read as configuration.
func validateNoControlChars(kind, value string) error {
	if strings.ContainsAny(value, "\n\r\x00\t") {
		return fmt.Errorf("%s must not contain control characters (newline, tab or NUL)", kind)
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
