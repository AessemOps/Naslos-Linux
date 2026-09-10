package shares

import (
	"fmt"
	"os"
	"path/filepath"
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

// AvailablePaths returns a list of available ZFS dataset paths.
func (m *Manager) AvailablePaths() ([]string, error) {
	entries, err := os.ReadDir(m.zfsBase)
	if err != nil {
		return nil, fmt.Errorf("reading ZFS base: %w", err)
	}

	var paths []string
	for _, entry := range entries {
		if entry.IsDir() {
			paths = append(paths, filepath.Join(m.zfsBase, entry.Name()))
		}
	}

	return paths, nil
}
