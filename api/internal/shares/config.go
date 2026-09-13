package shares

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// GenerateSambaConfig generates a Samba (smb.conf) configuration for all SMB shares.
func (m *Manager) GenerateSambaConfig() string {
	var sb strings.Builder

	sb.WriteString("[global]\n")
	sb.WriteString("   workgroup = NASLOS\n")
	sb.WriteString("   server string = Naslos\n")
	sb.WriteString("   security = user\n")
	sb.WriteString("   map to guest = Bad User\n")
	sb.WriteString("   log file = /var/log/samba/%m.log\n")
	sb.WriteString("   max log size = 1000\n")
	sb.WriteString("   dns proxy = no\n")
	// Keep the passdb and lock state on the mounted config directory so
	// provisioned accounts survive container restarts.
	sb.WriteString("   passdb backend = tdbsam:/etc/naslos/shares/private/passdb.tdb\n")
	sb.WriteString("   private dir = /etc/naslos/shares/private\n")
	sb.WriteString("   lock directory = /etc/naslos/shares/lock\n")
	sb.WriteString("   state directory = /etc/naslos/shares/state\n")
	sb.WriteString("   cache directory = /etc/naslos/shares/cache\n")
	sb.WriteString("   vfs objects = fruit streams_xattr\n")
	sb.WriteString("   fruit:time machine = yes\n")
	sb.WriteString("   fruit:model = MacSamba\n")
	sb.WriteString("   fruit:metadata = stream\n")
	sb.WriteString("   fruit:posix_rename = yes\n")
	sb.WriteString("   fruit:veto_appledouble = no\n")
	sb.WriteString("   fruit:nfs_aces = no\n")
	sb.WriteString("   fruit:wipe_intentionally_left_blank_rfork = yes\n")
	sb.WriteString("   fruit:delete_empty_adfiles = yes\n\n")

	for _, share := range m.shares {
		if share.Protocol != ProtocolSMB && share.Protocol != ProtocolAFP {
			continue
		}
		if !share.Enabled {
			continue
		}

		sb.WriteString(fmt.Sprintf("[%s]\n", share.Name))
		sb.WriteString(fmt.Sprintf("   path = %s\n", share.Path))
		sb.WriteString(fmt.Sprintf("   comment = %s\n", share.Description))

		if share.ReadOnly {
			sb.WriteString("   read only = yes\n")
		} else {
			sb.WriteString("   read only = no\n")
		}

		if share.Browseable {
			sb.WriteString("   browseable = yes\n")
		} else {
			sb.WriteString("   browseable = no\n")
		}

		if len(share.AllowedHosts) > 0 {
			sb.WriteString(fmt.Sprintf("   hosts allow = %s\n", strings.Join(share.AllowedHosts, " ")))
			sb.WriteString("   hosts deny = all\n")
		}

		if len(share.ValidUsers) > 0 {
			sb.WriteString(fmt.Sprintf("   valid users = %s\n", strings.Join(share.ValidUsers, " ")))
		}

		if share.TimeMachine {
			sb.WriteString("   fruit:time machine = yes\n")
			sb.WriteString("   fruit:time machine max size = 1T\n")
		}

		sb.WriteString("   create mask = 0664\n")
		sb.WriteString("   directory mask = 0775\n")
		sb.WriteString("   force user = root\n")
		sb.WriteString("   force group = root\n\n")
	}

	return sb.String()
}

// GenerateNFSExports generates NFS /etc/exports configuration.
func (m *Manager) GenerateNFSExports() string {
	var sb strings.Builder

	for _, share := range m.shares {
		if share.Protocol != ProtocolNFS {
			continue
		}
		if !share.Enabled {
			continue
		}

		sb.WriteString(share.Path)

		if len(share.AllowedHosts) > 0 {
			for _, host := range share.AllowedHosts {
				opts := "rw,sync,no_subtree_check"
				if share.ReadOnly {
					opts = "ro,sync,no_subtree_check"
				}
				sb.WriteString(fmt.Sprintf(" %s(%s)", host, opts))
			}
		} else {
			if share.ReadOnly {
				sb.WriteString(" *(ro,sync,no_subtree_check)")
			} else {
				sb.WriteString(" *(rw,sync,no_subtree_check)")
			}
		}
		sb.WriteString("\n")
	}

	return sb.String()
}

// AvailablePaths returns the directories directly under the ZFS base that are
// valid share targets. Returns an empty (non-nil) slice when the base is
// missing, so callers can distinguish "no datasets" from an error.
func (m *Manager) AvailablePaths() ([]string, error) {
	entries, err := os.ReadDir(m.zfsBase)
	if err != nil {
		if os.IsNotExist(err) {
			return []string{}, nil
		}
		return nil, fmt.Errorf("reading ZFS base: %w", err)
	}

	paths := make([]string, 0, len(entries))
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		// Skip hidden/system entries so internal bookkeeping dirs are not
		// offered as share targets.
		if strings.HasPrefix(entry.Name(), ".") {
			continue
		}
		paths = append(paths, filepath.Join(m.zfsBase, entry.Name()))
	}
	sort.Strings(paths)

	return paths, nil
}

// ConfigBundle is the set of rendered service configurations for the current
// share state. The API renders it and hands it to the privileged agent, which
// is the only component that can write to the Talos host filesystem.
type ConfigBundle struct {
	// SambaConf is the full smb.conf for SMB/Time Machine shares.
	SambaConf string `json:"sambaConf"`
	// NFSExports is the /etc/exports content for NFS shares.
	NFSExports string `json:"nfsExports"`
	// SambaUsers is the smbpasswd-format account file mirroring LDAP users
	// with their NT hashes (see SambaUserStore.RenderSMBPasswd).
	SambaUsers string `json:"sambaUsers"`
	// Revision changes whenever the share set changes, letting the servers
	// detect that a reload is needed.
	Revision string `json:"revision"`
	// ShareCount is the number of enabled shares represented above.
	ShareCount int `json:"shareCount"`
}

// RenderConfigBundle renders every service configuration for the current
// share state.
func (m *Manager) RenderConfigBundle() ConfigBundle {
	enabled := 0
	for _, s := range m.shares {
		if s.Enabled {
			enabled++
		}
	}

	return ConfigBundle{
		SambaConf:  m.GenerateSambaConfig(),
		NFSExports: m.GenerateNFSExports(),
		Revision:   m.revision(),
		ShareCount: enabled,
	}
}

// revision derives a stable content hash of the enabled share set. Consumers
// compare it against the last applied revision to decide whether to reload.
func (m *Manager) revision() string {
	h := sha256.New()
	for _, s := range m.List() {
		fmt.Fprintf(h, "%s\x00%s\x00%s\x00%t\x00%t\x00%t\x00%t\x00%s\x00%s\n",
			s.Name, s.Path, s.Protocol, s.Enabled, s.ReadOnly, s.Browseable,
			s.TimeMachine, strings.Join(s.AllowedHosts, ","), strings.Join(s.ValidUsers, ","))
	}
	return hex.EncodeToString(h.Sum(nil))[:16]
}
