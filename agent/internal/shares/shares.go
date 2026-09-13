// Package shares applies Naslos share service configuration on the Talos host.
//
// The naslos-api owns share definitions and renders the service configuration
// files (smb.conf, /etc/exports); this package is the privileged half that
// writes them into the host filesystem layout and tells the share services
// (running as separate pods with those host paths mounted) to reload.
//
// Layout on the host:
//
//	/var/lib/naslos/shares/smb.conf -> /etc/naslos/shares/smb.conf (samba)
//	/var/lib/naslos/shares/exports  -> /etc/naslos/shares/exports  (nfs)
//	/var/lib/naslos/shares/state/   -> samba private/lock/cache dirs
//
// Those containers watch the files for changes and reload themselves, so the
// agent only has to write atomically and never signals another workload.
package shares

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// hostRoot is where the Talos host filesystem is mounted into the agent pod.
const hostRoot = "/host"

// Paths relative to the host root.
const (
	// ConfigDir holds the rendered config files and samba's mutable state.
	ConfigDir = "/var/lib/naslos/shares"
	// SambaConfPath is the rendered smb.conf.
	SambaConfPath = ConfigDir + "/smb.conf"
	// NFSExportsPath is the rendered /etc/exports.
	NFSExportsPath = ConfigDir + "/exports"
	// RevisionPath records the revision currently applied on the host.
	RevisionPath = ConfigDir + "/revision"
)

// SMBUsersPath is the rendered smbpasswd-format account file. The naslos-samba
// container imports it so LDAP password changes reach Samba's passdb.
const SMBUsersPath = ConfigDir + "/smbusers"

// Config is the rendered configuration pushed by the API.
type Config struct {
	SambaConf  string
	NFSExports string
	SambaUsers string
	Revision   string
	ShareCount int
}

// Status reports the outcome of an apply, or the current on-host state.
type Status struct {
	Applied        bool   `json:"applied"`
	Revision       string `json:"revision"`
	SambaConfPath  string `json:"sambaConfPath"`
	NFSExportsPath string `json:"nfsExportsPath"`
	// SMBShareCount is the number of share blocks in the rendered smb.conf,
	// and NFSExportCount the number of export lines, so callers can confirm
	// the node actually received the expected shares.
	SMBShareCount  int      `json:"smbShareCount"`
	NFSExportCount int      `json:"nfsExportCount"`
	Messages       []string `json:"messages"`
	Error          string   `json:"error,omitempty"`
}

// Client applies share configuration on the host.
type Client struct {
	ctx context.Context
}

// NewClient creates a shares client.
func NewClient(ctx context.Context) *Client {
	return &Client{ctx: ctx}
}

// IsAvailable reports whether the agent can reach the Talos host filesystem.
func IsAvailable() bool {
	info, err := os.Stat(hostRoot)
	return err == nil && info.IsDir()
}

// hostPath maps an in-host path to the agent's mounted view.
func hostPath(p string) string {
	return filepath.Join(hostRoot, p)
}

// writeAtomic writes content to an in-host path via a temp file + rename so
// readers never observe a partially written file. mode sets the file
// permissions (use 0600 for files holding password hashes).
func writeAtomic(hostFilePath, content string, mode os.FileMode) error {
	target := hostPath(hostFilePath)
	if err := os.MkdirAll(filepath.Dir(target), 0755); err != nil {
		return fmt.Errorf("creating %s: %w", filepath.Dir(target), err)
	}

	tmp, err := os.CreateTemp(filepath.Dir(target), ".tmp-*")
	if err != nil {
		return fmt.Errorf("creating temp file for %s: %w", hostFilePath, err)
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)

	if _, err := tmp.WriteString(content); err != nil {
		tmp.Close()
		return fmt.Errorf("writing %s: %w", hostFilePath, err)
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return fmt.Errorf("syncing %s: %w", hostFilePath, err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("closing %s: %w", hostFilePath, err)
	}
	if err := os.Chmod(tmpName, mode); err != nil {
		return fmt.Errorf("chmod %s: %w", hostFilePath, err)
	}
	if err := os.Rename(tmpName, target); err != nil {
		return fmt.Errorf("replacing %s: %w", hostFilePath, err)
	}
	return nil
}

// readHostFile reads an in-host file, returning "" when it does not exist.
func readHostFile(hostFilePath string) string {
	data, err := os.ReadFile(hostPath(hostFilePath))
	if err != nil {
		return ""
	}
	return string(data)
}

// Apply writes the rendered configuration to the host and reports the result.
func (c *Client) Apply(cfg Config) (*Status, error) {
	if !IsAvailable() {
		return nil, fmt.Errorf("host filesystem not available at %s", hostRoot)
	}

	// Only rewrite files whose content changed: the serving containers reload
	// on mtime changes, so an unconditional rewrite on every API call would
	// cause needless reloads (and needless passdb imports).
	if existing := readHostFile(SambaConfPath); existing != cfg.SambaConf {
		if err := writeAtomic(SambaConfPath, cfg.SambaConf, 0644); err != nil {
			return nil, err
		}
	}
	if existing := readHostFile(NFSExportsPath); existing != cfg.NFSExports {
		if err := writeAtomic(NFSExportsPath, cfg.NFSExports, 0644); err != nil {
			return nil, err
		}
	}
	if existing := readHostFile(SMBUsersPath); existing != cfg.SambaUsers {
		// 0600: this file carries NT hashes.
		if err := writeAtomic(SMBUsersPath, cfg.SambaUsers, 0600); err != nil {
			return nil, err
		}
	}
	if err := writeAtomic(RevisionPath, cfg.Revision+"\n", 0644); err != nil {
		return nil, err
	}

	// smbd needs its private/state/lock dirs to exist before it starts.
	for _, dir := range []string{"private", "lock", "state", "cache"} {
		if err := os.MkdirAll(hostPath(ConfigDir+"/"+dir), 0755); err != nil {
			return nil, fmt.Errorf("creating samba %s dir: %w", dir, err)
		}
	}

	return c.Status()
}

// Status reports the configuration currently present on the host.
func (c *Client) Status() (*Status, error) {
	st := &Status{
		Revision:       strings.TrimSpace(readHostFile(RevisionPath)),
		SambaConfPath:  hostPath(SambaConfPath),
		NFSExportsPath: hostPath(NFSExportsPath),
		Messages:       []string{},
	}

	sambaConf := readHostFile(SambaConfPath)
	nfsExports := readHostFile(NFSExportsPath)

	if sambaConf == "" {
		st.Messages = append(st.Messages, "no smb.conf applied on this node yet")
	} else {
		st.Applied = true
		st.SMBShareCount = countSambaSections(sambaConf)
	}
	if nfsExports == "" {
		st.Messages = append(st.Messages, "no exports file applied on this node yet")
	} else {
		st.NFSExportCount = countExports(nfsExports)
	}

	return st, nil
}

// countSambaSections counts share sections ([name]) excluding [global].
func countSambaSections(conf string) int {
	count := 0
	for _, line := range strings.Split(conf, "\n") {
		line = strings.TrimSpace(line)
		if !strings.HasPrefix(line, "[") || !strings.HasSuffix(line, "]") {
			continue
		}
		if strings.EqualFold(line, "[global]") {
			continue
		}
		count++
	}
	return count
}

// countExports counts non-empty, non-comment export lines.
func countExports(exports string) int {
	count := 0
	for _, line := range strings.Split(exports, "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		count++
	}
	return count
}
