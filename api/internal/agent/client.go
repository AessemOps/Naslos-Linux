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
	"net/url"
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
	// stream is the same client without a total timeout, for `zfs send`/`receive`
	// which run far longer than a control-plane call. It carries the token
	// transport too: a package-level client with a default transport was used
	// here once, which silently sent no Authorization header and made every
	// backup fail with the agent's 401 under auth.
	stream *http.Client
}

// authTransport injects the shared agent bearer token on every request. Doing it
// in the transport (rather than at each call site) means the streaming
// send/receive paths, which build their own requests, cannot be forgotten.
type authTransport struct {
	token string
	base  http.RoundTripper
}

func (t *authTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	base := t.base
	if base == nil {
		base = http.DefaultTransport
	}
	if t.token == "" {
		return base.RoundTrip(req)
	}
	// Clone: the caller's request must not be mutated.
	cloned := req.Clone(req.Context())
	cloned.Header.Set("Authorization", "Bearer "+t.token)
	return base.RoundTrip(cloned)
}

// NewClient creates an agent client for baseURL (e.g.
// http://naslos-agent.naslos.svc.cluster.local:9090). token is the shared secret
// from the naslos-agent-auth Secret; the agent refuses every request but /health
// without it. An empty token is a deliberate local-development posture only.
func NewClient(baseURL, token string) *Client {
	return &Client{
		baseURL: strings.TrimSuffix(baseURL, "/"),
		http: &http.Client{
			Timeout:   DefaultTimeout,
			Transport: &authTransport{token: token},
		},
		stream: &http.Client{
			Transport: &authTransport{token: token},
		},
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
	// GaneshaConf is the NFS-Ganesha configuration content (NFS is served by
	// Ganesha on Talos, which has no kernel NFS server).
	GaneshaConf string `json:"ganeshaConf"`
	// SambaUsers is the smbpasswd-format account file whose NT hashes are
	// imported into Samba's passdb (keeps SMB logins in step with LDAP).
	SambaUsers string `json:"sambaUsers"`
	// NSSPasswd / NSSGroup / NSSShadow are extrausers-format files written to
	// the node so the serving container can resolve LDAP users via NSS.
	NSSPasswd string `json:"nssPasswd"`
	NSSGroup  string `json:"nssGroup"`
	NSSShadow string `json:"nssShadow"`
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
	// SambaConfPath / GaneshaConfPath are the rendered file locations.
	SambaConfPath   string `json:"sambaConfPath"`
	GaneshaConfPath string `json:"ganeshaConfPath"`
	// SMBShareCount is the number of share sections the node's rendered
	// smb.conf contains, and NFSExportCount the number of export lines, so
	// callers can confirm the node received the expected shares.
	SMBShareCount  int `json:"smbShareCount"`
	NFSExportCount int `json:"nfsExportCount"`
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

// Dataset mirrors the agent's Dataset JSON shape (agent/internal/zfs/dataset.go).
type Dataset struct {
	Name       string `json:"name"`
	Used       string `json:"used,omitempty"`
	Avail      string `json:"avail,omitempty"`
	Refer      string `json:"refer,omitempty"`
	Mountpoint string `json:"mountpoint"`
	// UsedBytes is the dataset's used space in exact bytes. It is what the buddy
	// send path compares a stream against to catch a backup that captured nothing
	// (AV-8): a dataset the agent's mount namespace cannot see still reports its
	// real UsedBytes, so a stream far smaller than that is a red flag even though
	// the dataset reads as mounted.
	UsedBytes int64 `json:"usedBytes"`
	// Mounted is the mount state as the *agent* sees it (host mount namespace),
	// which is where `zfs send` runs. A dataset mounted only inside a pod shows
	// false here, and sending it would capture an empty dataset.
	Mounted bool `json:"mounted"`
}

// ListDatasets returns every dataset on the node with its mountpoint.
func (c *Client) ListDatasets() ([]Dataset, error) {
	req, err := http.NewRequest(http.MethodGet, c.baseURL+"/api/v1/datasets", nil)
	if err != nil {
		return nil, fmt.Errorf("creating list-datasets request: %w", err)
	}
	var datasets []Dataset
	if err := c.do(req, &datasets); err != nil {
		return nil, err
	}
	if datasets == nil {
		datasets = []Dataset{}
	}
	return datasets, nil
}

// CreateDataset creates a dataset inside an existing pool. name is relative to
// the pool ("media", "photos/2026"); options is the safe property subset the
// agent accepts (compression, quota, recordsize, atime, copies, readonly).
func (c *Client) CreateDataset(pool, name string, options map[string]string) error {
	body, err := json.Marshal(map[string]interface{}{"name": name, "options": options})
	if err != nil {
		return fmt.Errorf("encoding create-dataset request: %w", err)
	}

	req, err := http.NewRequest(http.MethodPost,
		c.baseURL+"/api/v1/datasets/"+url.PathEscape(pool), bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("creating create-dataset request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	return c.do(req, nil)
}

// DestroyDataset destroys a dataset (pool/name[/…]). recursive must be set
// explicitly for a dataset with children or snapshots.
func (c *Client) DestroyDataset(name string, recursive bool) error {
	target := c.baseURL + "/api/v1/datasets/" + escapeDatasetPath(name)
	if recursive {
		target += "?recursive=true"
	}

	req, err := http.NewRequest(http.MethodDelete, target, nil)
	if err != nil {
		return fmt.Errorf("creating destroy-dataset request: %w", err)
	}
	return c.do(req, nil)
}

// AddPoolVDev attaches disks to an existing pool: `zpool add [-f] <pool>
// [<topology>] <disk>…`. force allows overwriting an unrecognised signature.
func (c *Client) AddPoolVDev(pool, topology string, disks []string, force bool) error {
	body, err := json.Marshal(map[string]interface{}{
		"disks": disks, "topology": topology, "force": force,
	})
	if err != nil {
		return fmt.Errorf("encoding add-devices request: %w", err)
	}

	req, err := http.NewRequest(http.MethodPost,
		c.baseURL+"/api/v1/pools/"+url.PathEscape(pool)+"/devices", bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("creating add-devices request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	return c.do(req, nil)
}

// escapeDatasetPath escapes each component of a dataset path but keeps the
// separators, so pool/name reaches the agent as two path segments.
func escapeDatasetPath(name string) string {
	parts := strings.Split(name, "/")
	for i, p := range parts {
		parts[i] = url.PathEscape(p)
	}
	return strings.Join(parts, "/")
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

// ListShareFolders returns the subfolders of a folder inside the share datasets.
func (c *Client) ListShareFolders(path string) ([]string, error) {
	req, err := http.NewRequest(http.MethodGet,
		c.baseURL+"/api/v1/shares/folders?path="+url.QueryEscape(path), nil)
	if err != nil {
		return nil, fmt.Errorf("creating list-folders request: %w", err)
	}

	var payload struct {
		Folders []string `json:"folders"`
	}
	if err := c.do(req, &payload); err != nil {
		return nil, err
	}
	if payload.Folders == nil {
		payload.Folders = []string{}
	}
	return payload.Folders, nil
}

// CreateShareFolder creates a folder inside path and returns the new path.
func (c *Client) CreateShareFolder(path, name string) (string, error) {
	body, err := json.Marshal(map[string]string{"path": path, "name": name})
	if err != nil {
		return "", fmt.Errorf("encoding create-folder request: %w", err)
	}

	req, err := http.NewRequest(http.MethodPost, c.baseURL+"/api/v1/shares/folders", bytes.NewReader(body))
	if err != nil {
		return "", fmt.Errorf("creating create-folder request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")

	var payload map[string]string
	if err := c.do(req, &payload); err != nil {
		return "", err
	}
	return payload["path"], nil
}

// DeleteShareFolder removes an empty folder inside the share datasets.
func (c *Client) DeleteShareFolder(path string) error {
	req, err := http.NewRequest(http.MethodDelete,
		c.baseURL+"/api/v1/shares/folders?path="+url.QueryEscape(path), nil)
	if err != nil {
		return fmt.Errorf("creating delete-folder request: %w", err)
	}
	return c.do(req, nil)
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
