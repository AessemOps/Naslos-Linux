// Package server provides the naslos-agent HTTP API server.
// The naslos-api calls this server to execute ZFS operations on each node.
package server

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"log"
	"net/http"
	"strings"
	"time"

	"github.com/AessemOps/Naslos-Linux/agent/internal/shares"
	"github.com/AessemOps/Naslos-Linux/agent/internal/zfs"
)

// Body caps (PF-M6). A JSON control request is tiny; a `zfs receive` stream is
// the only large body and gets a high but finite cap so one request cannot fill
// the node's disk through the agent.
const (
	maxJSONBodyBytes   = 1 << 20 // 1 MiB
	maxStreamBodyBytes = 1 << 40 // 1 TiB
)

// Options configure the agent's authentication. The agent is a privileged,
// hostNetwork DaemonSet whose endpoints destroy pools and receive datasets, so a
// caller must prove it is the API (NAS-002).
type Options struct {
	// AuthToken is the shared bearer token every request except /health must
	// present. The API reads the same value from the same Secret.
	AuthToken string
	// TLSCertFile / TLSKeyFile, when both set, make the agent serve HTTPS. The
	// agent is hostNetwork and the bearer token would otherwise cross the LAN in
	// cleartext (PF-M5). Leave empty for plain HTTP (local development).
	TLSCertFile string
	TLSKeyFile  string
}

// Server is the agent's HTTP server.
type Server struct {
	addr   string
	zfs    *zfs.Client
	shares *shares.Client
	// backup is the streaming slice of the ZFS client (send/receive/estimate).
	// Nil when the agent runs degraded (no ZFS on the host).
	backup backupZFS
	// authToken is the shared API token the API presents; there is no opt-out.
	authToken string
	// tlsCertFile / tlsKeyFile make the agent serve HTTPS when both are set.
	tlsCertFile string
	tlsKeyFile  string
	router      *http.ServeMux
	server      *http.Server
}

// New creates a new agent server.
func New(addr string, zfsClient *zfs.Client, sharesClient *shares.Client, opts Options) *Server {
	s := &Server{
		addr:        addr,
		zfs:         zfsClient,
		shares:      sharesClient,
		authToken:   opts.AuthToken,
		tlsCertFile: opts.TLSCertFile,
		tlsKeyFile:  opts.TLSKeyFile,
		router:      http.NewServeMux(),
	}
	if zfsClient != nil {
		s.backup = zfsClient
	}
	s.routes()
	return s
}

// requireAuth wraps a handler with the bearer-token check. It fails closed: an
// agent started without a configured token refuses everything (main refuses to
// start in that case, this is the belt to that braces).
func (s *Server) requireAuth(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		const prefix = "Bearer "
		header := r.Header.Get("Authorization")
		if s.authToken == "" || !strings.HasPrefix(header, prefix) ||
			subtle.ConstantTimeCompare([]byte(strings.TrimPrefix(header, prefix)), []byte(s.authToken)) != 1 {
			writeError(w, http.StatusUnauthorized, "missing or invalid agent token")
			return
		}
		// Cap the body here so every authenticated route is covered (PF-M6).
		// The receive stream is the one large body; everything else is JSON and
		// is capped hard.
		limit := int64(maxJSONBodyBytes)
		if strings.HasPrefix(r.URL.Path, "/api/v1/zfs/receive/") {
			limit = maxStreamBodyBytes
		}
		r.Body = http.MaxBytesReader(w, r.Body, limit)
		next.ServeHTTP(w, r)
	})
}

// routes registers all agent API routes.
//
// /health is public so the DaemonSet's probes keep working; every other route
// lives on the `api` mux, which is wrapped in the token check. A new route added
// to `api` is therefore authenticated by default.
func (s *Server) routes() {
	s.router.HandleFunc("/health", s.handleHealth)

	api := http.NewServeMux()
	api.HandleFunc("/api/v1/pools", s.handlePools)
	api.HandleFunc("/api/v1/pools/import", s.handlePoolImport)
	api.HandleFunc("/api/v1/pools/", s.handlePoolDetail)
	api.HandleFunc("/api/v1/datasets", s.handleDatasetsAll)
	api.HandleFunc("/api/v1/datasets/", s.handleDatasets)
	api.HandleFunc("/api/v1/snapshots/", s.handleSnapshots)
	api.HandleFunc("/api/v1/shares/config", s.handleSharesConfig)
	api.HandleFunc("/api/v1/shares/status", s.handleSharesStatus)
	// Folder management for share paths (the API's dataset mount is read-only).
	api.HandleFunc("/api/v1/shares/folders", s.handleShareFolders)
	// Backup streams (FR-BUD): `zfs send`/`receive` as pipes rather than
	// captured output, which is what lets an instance back itself up.
	api.HandleFunc("/api/v1/zfs/send/", s.handleSendStream)
	api.HandleFunc("/api/v1/zfs/receive/", s.handleReceiveStream)
	api.HandleFunc("/api/v1/zfs/snapshots/", s.handleBackupSnapshots)

	s.router.Handle("/api/", s.requireAuth(api))
}

// zfsUnavailable reports whether the agent runs in degraded mode (no ZFS on
// the host) and, in that case, writes a 503 response. Call at the top of
// every ZFS-backed handler.
func (s *Server) zfsUnavailable(w http.ResponseWriter) bool {
	if s.zfs == nil {
		writeError(w, http.StatusServiceUnavailable,
			"ZFS is not available on this node (agent running in degraded mode)")
		return true
	}
	return false
}

// backupUnavailable is zfsUnavailable for the streaming backup endpoints, which
// need the streaming client rather than the buffered one.
func (s *Server) backupUnavailable(w http.ResponseWriter) bool {
	if s.backup == nil {
		writeError(w, http.StatusServiceUnavailable,
			"ZFS streaming is not available on this node (agent running in degraded mode)")
		return true
	}
	return false
}

// Start starts the HTTP server.
func (s *Server) Start() error {
	s.server = &http.Server{
		Addr:    s.addr,
		Handler: s.router,
		// Bound slow/silent clients (gosec G112): without a header deadline one
		// connection can hold a worker open indefinitely.
		ReadHeaderTimeout: 10 * time.Second,
		// A control request must not sit half-read forever. The streaming
		// receive route clears this deadline for its own connection (PF-M6).
		ReadTimeout: 30 * time.Second,
		IdleTimeout: 120 * time.Second,
		// WriteTimeout stays 0: the send stream is a long-lived response.
	}
	if s.tlsCertFile != "" && s.tlsKeyFile != "" {
		return s.server.ListenAndServeTLS(s.tlsCertFile, s.tlsKeyFile)
	}
	return s.server.ListenAndServe()
}

// Shutdown gracefully shuts down the server.
func (s *Server) Shutdown(ctx context.Context) error {
	return s.server.Shutdown(ctx)
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

func (s *Server) handlePools(w http.ResponseWriter, r *http.Request) {
	if s.zfsUnavailable(w) {
		return
	}
	switch r.Method {
	case http.MethodGet:
		pools, err := s.zfs.Pools()
		if err != nil {
			writeClientError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, pools)
	case http.MethodPost:
		var cfg zfs.PoolConfig
		if err := json.NewDecoder(r.Body).Decode(&cfg); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if err := s.zfs.CreatePool(cfg); err != nil {
			writeClientError(w, err)
			return
		}
		writeJSON(w, http.StatusCreated, map[string]string{"status": "pool created", "name": cfg.Name})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

func (s *Server) handlePoolImport(w http.ResponseWriter, r *http.Request) {
	if s.zfsUnavailable(w) {
		return
	}
	switch r.Method {
	case http.MethodGet:
		// List pools available for import.
		pools, err := s.zfs.ListImportable()
		if err != nil {
			writeClientError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, pools)
	case http.MethodPost:
		// Import pool(s). Body: {"name": "tank"} or empty {} for all.
		var req struct {
			Name string `json:"name"`
		}
		json.NewDecoder(r.Body).Decode(&req)
		if err := s.zfs.ImportPool(req.Name); err != nil {
			writeClientError(w, err)
			return
		}
		if req.Name == "" {
			writeJSON(w, http.StatusOK, map[string]string{"status": "all pools imported"})
		} else {
			writeJSON(w, http.StatusOK, map[string]string{"status": "pool imported", "name": req.Name})
		}
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

func (s *Server) handlePoolDetail(w http.ResponseWriter, r *http.Request) {
	if s.zfsUnavailable(w) {
		return
	}
	pool := r.URL.Path[len("/api/v1/pools/"):]

	// Dispatch /devices: attaching a vdev (adding disks) to an existing pool.
	if strings.HasSuffix(pool, "/devices") {
		s.handlePoolDevices(w, r, strings.TrimSuffix(pool, "/devices"))
		return
	}

	switch r.Method {
	case http.MethodGet:
		health, err := s.zfs.PoolHealth(pool)
		if err != nil {
			writeClientError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, health)
	case http.MethodDelete:
		if err := s.zfs.DestroyPool(pool); err != nil {
			writeClientError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "pool destroyed", "name": pool})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handlePoolDevices adds a vdev (disks) to an existing pool.
//
//	GET  /api/v1/pools/{pool}/devices → disks that could be added (not in any pool)
//	POST /api/v1/pools/{pool}/devices {"disks":[…],"topology":"mirror","force":false}
//
// "force" is deliberately a separate, opt-in flag: it lets `zpool add -f`
// overwrite an unrecognised signature, which is unrecoverable.
func (s *Server) handlePoolDevices(w http.ResponseWriter, r *http.Request, pool string) {
	if s.zfsUnavailable(w) {
		return
	}
	if err := zfs.ValidatePoolName(pool); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	switch r.Method {
	case http.MethodGet:
		free, err := s.zfs.FreeDisks()
		if err != nil {
			writeClientError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, map[string]interface{}{"pool": pool, "disks": free})

	case http.MethodPost:
		var req struct {
			Disks    []string `json:"disks"`
			Topology string   `json:"topology"`
			Force    bool     `json:"force"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if err := s.zfs.AddVDev(pool, req.Topology, req.Disks, req.Force); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		log.Printf("added %d disk(s) to pool %s (topology %q, force %v)", len(req.Disks), pool, req.Topology, req.Force)
		writeJSON(w, http.StatusCreated, map[string]interface{}{
			"status":   "vdev added",
			"pool":     pool,
			"topology": req.Topology,
			"disks":    req.Disks,
		})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleDatasets handles dataset operations for a pool.
//
//	GET    /api/v1/datasets/{pool}          → datasets in a pool
//	POST   /api/v1/datasets/{pool}          → create {name, options}
//	DELETE /api/v1/datasets/{pool}/{name…}  → destroy (recursive=true to force)
func (s *Server) handleDatasets(w http.ResponseWriter, r *http.Request) {
	if s.zfsUnavailable(w) {
		return
	}
	rest := r.URL.Path[len("/api/v1/datasets/"):]
	if rest == "" {
		writeError(w, http.StatusBadRequest, "pool name required")
		return
	}

	switch r.Method {
	case http.MethodGet:
		datasets, err := s.zfs.Datasets(rest)
		if err != nil {
			writeClientError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, datasets)

	case http.MethodPost:
		var req struct {
			Name    string            `json:"name"`
			Options map[string]string `json:"options"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		datasetName := rest + "/" + req.Name
		if err := s.zfs.CreateDataset(datasetName, req.Options); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		log.Printf("created dataset %s", datasetName)
		writeJSON(w, http.StatusCreated, map[string]string{"status": "dataset created", "name": datasetName})

	case http.MethodDelete:
		// rest is the full dataset path here (pool/name[/…]).
		recursive := r.URL.Query().Get("recursive") == "true"
		if err := s.zfs.DestroyDataset(rest, recursive); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		log.Printf("destroyed dataset %s (recursive=%v)", rest, recursive)
		writeJSON(w, http.StatusOK, map[string]string{"status": "dataset destroyed", "name": rest})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

func (s *Server) handleSnapshots(w http.ResponseWriter, r *http.Request) {
	if s.zfsUnavailable(w) {
		return
	}
	dataset := r.URL.Path[len("/api/v1/snapshots/"):]
	switch r.Method {
	case http.MethodGet:
		snaps, err := s.zfs.Snapshots(dataset)
		if err != nil {
			writeClientError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, snaps)
	case http.MethodPost:
		var req struct {
			Name string `json:"name"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if err := s.zfs.Snapshot(dataset, req.Name); err != nil {
			writeClientError(w, err)
			return
		}
		writeJSON(w, http.StatusCreated, map[string]string{"status": "snapshot created"})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

func writeJSON(w http.ResponseWriter, status int, data interface{}) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(data)
}

func writeError(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, map[string]string{"error": msg})
}

// writeClientError answers a backend client error with the right status: input
// the caller can fix (a bad name, disk, topology or option) is a 400, anything
// else is a node-side failure and stays a 500 (NAS-003).
func writeClientError(w http.ResponseWriter, err error) {
	status := http.StatusInternalServerError
	var invalid *zfs.ValidationError
	if errors.As(err, &invalid) {
		status = http.StatusBadRequest
	}
	writeError(w, status, err.Error())
}

var _ = log.Printf // suppress unused import
