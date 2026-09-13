// Package agent provides a typed HTTP client for the naslos-agent DaemonSet.
//
// The agent runs one pod per node (hostNetwork :9090, fronted by the
// headless naslos-agent Service) and executes zpool/zfs via chroot /host.
// The API uses this client to implement GET/POST/DELETE /api/volumes/zfs.
package agent

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

// DefaultBaseURLPattern is used when AGENT_BASE_URL is unset; %s is the
// namespace (the agent Service lives in the same namespace as the API).
const DefaultBaseURLPattern = "http://naslos-agent.%s.svc.cluster.local:9090"

// DefaultTimeout covers slow operations: wipefs + zpool create on real
// spinning disks can take minutes, and the agent call is synchronous.
const DefaultTimeout = 180 * time.Second

// Pool mirrors the agent's Pool JSON shape (agent/internal/zfs/pool.go).
type Pool struct {
	Name       string   `json:"name"`
	Size       string   `json:"size"`
	Alloc      string   `json:"alloc"`
	Free       string   `json:"free"`
	Health     string   `json:"health"`
	Topology   string   `json:"topology"`
	Disks      []string `json:"disks"`
	Mountpoint string   `json:"mountpoint"`
}

// CreatePoolRequest mirrors the agent's PoolConfig JSON shape.
type CreatePoolRequest struct {
	Name     string            `json:"name"`
	Topology string            `json:"topology"`
	Disks    []string          `json:"disks"`
	Cache    string            `json:"cache"`
	Options  map[string]string `json:"options"`
}

// Error is an agent-side failure carrying the upstream HTTP status so the
// API handler can forward 503 (degraded, no ZFS on host) instead of
// collapsing everything into 500.
type Error struct {
	Status  int
	Message string
}

func (e *Error) Error() string {
	return fmt.Sprintf("agent: %s", e.Message)
}

// Client is a thin HTTP client for one agent endpoint.
type Client struct {
	baseURL string
	http    *http.Client
}

// NewClient creates an agent client for baseURL (e.g.
// http://naslos-agent.naslos.svc.cluster.local:9090).
func NewClient(baseURL string) *Client {
	return &Client{
		baseURL: strings.TrimSuffix(baseURL, "/"),
		http:    &http.Client{Timeout: DefaultTimeout},
	}
}

// do performs req and decodes a JSON body into out (if non-nil) on success.
func (c *Client) do(req *http.Request, out interface{}) error {
	resp, err := c.http.Do(req)
	if err != nil {
		return fmt.Errorf("contacting agent at %s: %w", c.baseURL, err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return fmt.Errorf("reading agent response: %w", err)
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		msg := strings.TrimSpace(string(body))
		// Agent errors are {"error": "..."} JSON; surface the inner message.
		var payload map[string]string
		if json.Unmarshal(body, &payload) == nil && payload["error"] != "" {
			msg = payload["error"]
		}
		return &Error{Status: resp.StatusCode, Message: msg}
	}

	if out != nil && len(bytes.TrimSpace(body)) > 0 {
		if err := json.Unmarshal(body, out); err != nil {
			return fmt.Errorf("decoding agent response: %w", err)
		}
	}
	return nil
}

// SharesConfigRequest is the desired share configuration pushed to the node.
// The agent renders these into the service config files it manages; the API
// renders the file contents because the share definitions live there.
type SharesConfigRequest struct {
	// SambaConf is the full smb.conf content.
	SambaConf string `json:"sambaConf"`
	// NFSExports is the full /etc/exports content.
	NFSExports string `json:"nfsExports"`
	// SambaUsers is the smbpasswd-format account file whose NT hashes are
	// imported into Samba's passdb (keeps SMB logins in step with LDAP).
	SambaUsers string `json:"sambaUsers"`
	// Revision is an opaque content hash used to skip redundant reloads.
	Revision string `json:"revision"`
	// ShareCount is the number of enabled shares, for status reporting.
	ShareCount int `json:"shareCount"`
}

// SharesConfigStatus is the agent's report of what it applied on the host.
type SharesConfigStatus struct {
	// Applied indicates the configuration was written successfully.
	Applied bool `json:"applied"`
	// Revision is the revision currently on disk.
	Revision string `json:"revision"`
	// SambaConfPath / NFSExportsPath are the rendered file locations.
	SambaConfPath  string `json:"sambaConfPath"`
	NFSExportsPath string `json:"nfsExportsPath"`
	// SambaRunning / NFSRunning report whether the share services are
	// currently active on the node.
	SambaRunning bool `json:"sambaRunning"`
	NFSRunning   bool `json:"nfsRunning"`
	// SambaReloaded / NFSReloaded report whether this call triggered a reload.
	SambaReloaded bool `json:"sambaReloaded"`
	NFSReloaded   bool `json:"nfsReloaded"`
	// SambaTestOutput / NFSTestOutput hold the validation command output.
	SambaTestOutput string `json:"sambaTestOutput"`
	NFSTestOutput   string `json:"nfsTestOutput"`
	// Messages collects human-readable notes/errors from the apply step.
	Messages []string `json:"messages"`
	// Error is set when the configuration could not be applied.
	Error string `json:"error,omitempty"`
}

// ApplySharesConfig pushes the rendered share configuration to the agent,
// which writes it into the share services' config directory on the node.
func (c *Client) ApplySharesConfig(req SharesConfigRequest) (*SharesConfigStatus, error) {
	body, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("encoding shares config: %w", err)
	}

	httpReq, err := http.NewRequest(http.MethodPut, c.baseURL+"/api/v1/shares/config", bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("creating shares-config request: %w", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")

	var status SharesConfigStatus
	if err := c.do(httpReq, &status); err != nil {
		return nil, err
	}
	return &status, nil
}

// GetSharesStatus reports what share configuration and services the node has.
func (c *Client) GetSharesStatus() (*SharesConfigStatus, error) {
	req, err := http.NewRequest(http.MethodGet, c.baseURL+"/api/v1/shares/status", nil)
	if err != nil {
		return nil, fmt.Errorf("creating shares-status request: %w", err)
	}
	var status SharesConfigStatus
	if err := c.do(req, &status); err != nil {
		return nil, err
	}
	return &status, nil
}

// ListPools returns all ZFS pools known to the agent.
func (c *Client) ListPools() ([]Pool, error) {
	req, err := http.NewRequest(http.MethodGet, c.baseURL+"/api/v1/pools", nil)
	if err != nil {
		return nil, fmt.Errorf("creating list-pools request: %w", err)
	}
	var pools []Pool
	if err := c.do(req, &pools); err != nil {
		return nil, err
	}
	if pools == nil {
		pools = []Pool{}
	}
	return pools, nil
}

// CreatePool creates a ZFS pool via the agent.
func (c *Client) CreatePool(req CreatePoolRequest) error {
	body, err := json.Marshal(req)
	if err != nil {
		return fmt.Errorf("encoding pool request: %w", err)
	}
	httpReq, err := http.NewRequest(http.MethodPost, c.baseURL+"/api/v1/pools", bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("creating create-pool request: %w", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")
	return c.do(httpReq, nil)
}

// DeletePool destroys a ZFS pool via the agent.
func (c *Client) DeletePool(name string) error {
	httpReq, err := http.NewRequest(http.MethodDelete, c.baseURL+"/api/v1/pools/"+name, nil)
	if err != nil {
		return fmt.Errorf("creating delete-pool request: %w", err)
	}
	return c.do(httpReq, nil)
}

// PoolStatus returns detailed `zpool status` output for a pool.
func (c *Client) PoolStatus(name string) (string, error) {
	httpReq, err := http.NewRequest(http.MethodGet, c.baseURL+"/api/v1/pools/"+name, nil)
	if err != nil {
		return "", fmt.Errorf("creating pool-status request: %w", err)
	}
	var payload map[string]string
	if err := c.do(httpReq, &payload); err != nil {
		return "", err
	}
	return payload["status"], nil
}

// ImportablePool is a pool that exists on disk but is not currently imported.
type ImportablePool struct {
	Name     string   `json:"name"`
	State    string   `json:"state"`
	Topology string   `json:"topology"`
	Disks    []string `json:"disks"`
}

// ListImportablePools returns pools available for import.
func (c *Client) ListImportablePools() ([]ImportablePool, error) {
	req, err := http.NewRequest(http.MethodGet, c.baseURL+"/api/v1/pools/import", nil)
	if err != nil {
		return nil, fmt.Errorf("creating list-importable request: %w", err)
	}
	var pools []ImportablePool
	if err := c.do(req, &pools); err != nil {
		return nil, err
	}
	if pools == nil {
		pools = []ImportablePool{}
	}
	return pools, nil
}

// ImportPool imports an existing pool via the agent.
func (c *Client) ImportPool(name string) error {
	body, err := json.Marshal(map[string]string{"name": name})
	if err != nil {
		return fmt.Errorf("encoding import request: %w", err)
	}
	httpReq, err := http.NewRequest(http.MethodPost, c.baseURL+"/api/v1/pools/import", bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("creating import-pool request: %w", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")
	return c.do(httpReq, nil)
}

// PoolHealth returns structured health data for a pool.
func (c *Client) PoolHealth(name string) (*PoolHealth, error) {
	httpReq, err := http.NewRequest(http.MethodGet, c.baseURL+"/api/v1/pools/"+name, nil)
	if err != nil {
		return nil, fmt.Errorf("creating pool-health request: %w", err)
	}
	var health PoolHealth
	if err := c.do(httpReq, &health); err != nil {
		return nil, err
	}
	return &health, nil
}

// PoolHealth mirrors the agent agent's PoolHealth JSON shape.
type PoolHealth struct {
	Name    string       `json:"name"`
	State   string       `json:"state"`
	Scan    string       `json:"scan"`
	Errors  string       `json:"errors"`
	Config  []PoolDevice `json:"config"`
	IOStats PoolIOStats  `json:"ioStats"`
}

// PoolDevice is a single device in the pool config tree.
type PoolDevice struct {
	Name    string       `json:"name"`
	State   string       `json:"state"`
	Read    string       `json:"read"`
	Write   string       `json:"write"`
	Cksum   string       `json:"cksum"`
	Devices []PoolDevice `json:"devices,omitempty"`
}

// PoolIOStats holds `zpool iostat` counters.
type PoolIOStats struct {
	ReadOps  string `json:"readOps"`
	WriteOps string `json:"writeOps"`
	ReadBW   string `json:"readBW"`
	WriteBW  string `json:"writeBW"`
}
