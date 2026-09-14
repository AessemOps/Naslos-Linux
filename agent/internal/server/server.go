// Package server provides the naslos-agent HTTP API server.
// The naslos-api calls this server to execute ZFS operations on each node.
package server

import (
	"context"
	"encoding/json"
	"log"
	"net/http"

	"github.com/AessemOps/Naslos-Linux/agent/internal/shares"
	"github.com/AessemOps/Naslos-Linux/agent/internal/zfs"
)

// Server is the agent's HTTP server.
type Server struct {
	addr   string
	zfs    *zfs.Client
	shares *shares.Client
	router *http.ServeMux
	server *http.Server
}

// New creates a new agent server.
func New(addr string, zfsClient *zfs.Client, sharesClient *shares.Client) *Server {
	s := &Server{
		addr:   addr,
		zfs:    zfsClient,
		shares: sharesClient,
		router: http.NewServeMux(),
	}
	s.routes()
	return s
}

// routes registers all agent API routes.
func (s *Server) routes() {
	s.router.HandleFunc("/health", s.handleHealth)
	s.router.HandleFunc("/api/v1/pools", s.handlePools)
	s.router.HandleFunc("/api/v1/pools/import", s.handlePoolImport)
	s.router.HandleFunc("/api/v1/pools/", s.handlePoolDetail)
	s.router.HandleFunc("/api/v1/datasets", s.handleDatasetsAll)
	s.router.HandleFunc("/api/v1/datasets/", s.handleDatasets)
	s.router.HandleFunc("/api/v1/snapshots/", s.handleSnapshots)
	s.router.HandleFunc("/api/v1/shares/config", s.handleSharesConfig)
	s.router.HandleFunc("/api/v1/shares/status", s.handleSharesStatus)
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

// Start starts the HTTP server.
func (s *Server) Start() error {
	s.server = &http.Server{
		Addr:    s.addr,
		Handler: s.router,
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
			writeError(w, http.StatusInternalServerError, err.Error())
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
			writeError(w, http.StatusInternalServerError, err.Error())
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
			writeError(w, http.StatusInternalServerError, err.Error())
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
			writeError(w, http.StatusInternalServerError, err.Error())
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
	switch r.Method {
	case http.MethodGet:
		health, err := s.zfs.PoolHealth(pool)
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, health)
	case http.MethodDelete:
		if err := s.zfs.DestroyPool(pool); err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "pool destroyed", "name": pool})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

func (s *Server) handleDatasets(w http.ResponseWriter, r *http.Request) {
	if s.zfsUnavailable(w) {
		return
	}
	pool := r.URL.Path[len("/api/v1/datasets/"):]
	switch r.Method {
	case http.MethodGet:
		datasets, err := s.zfs.Datasets(pool)
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
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
		datasetName := pool + "/" + req.Name
		if err := s.zfs.CreateDataset(datasetName, req.Options); err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusCreated, map[string]string{"status": "dataset created", "name": datasetName})
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
			writeError(w, http.StatusInternalServerError, err.Error())
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
			writeError(w, http.StatusInternalServerError, err.Error())
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

var _ = log.Printf // suppress unused import
