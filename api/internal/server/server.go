// Package server provides the NasOS HTTP API server.
package server

import (
	"context"
	"encoding/json"
	"log"
	"net/http"
	"time"

	"github.com/nasos/nasos/api/internal/catalog"
	"github.com/nasos/nasos/api/internal/helm"
	"github.com/nasos/nasos/api/internal/metrics"
	"github.com/nasos/nasos/api/internal/notifications"
	"github.com/nasos/nasos/api/internal/shares"
	"github.com/nasos/nasos/api/internal/talos"
)

// Server is the HTTP API server.
type Server struct {
	addr          string
	talos         *talos.Client
	helm          *helm.Client
	catalog       *catalog.Catalog
	shares        *shares.Manager
	metrics       *metrics.Manager
	notifications *notifications.Manager
	kubeconfig    string
	router        *http.ServeMux
	server        *http.Server
}

// New creates a new server.
func New(addr string, tc *talos.Client) *Server {
	// Initialize Helm client for the nasos namespace
	helmClient := helm.NewClient("nasos")

	// Initialize app catalog (built-in)
	c := catalog.New("")

	// Initialize share manager
	shareManager := shares.NewManager("")

	// Initialize metrics manager
	metricsManager := metrics.NewManager()

	// Initialize notification manager
	notifManager := notifications.NewManager("")

	s := &Server{
		addr:          addr,
		talos:         tc,
		helm:          helmClient,
		catalog:       c,
		shares:        shareManager,
		metrics:       metricsManager,
		notifications: notifManager,
		router:        http.NewServeMux(),
	}
	s.routes()
	return s
}

// routes registers all API routes.
func (s *Server) routes() {
	// Health
	s.router.HandleFunc("/api/health", s.handleHealth)

	// Catalog (app store)
	s.router.HandleFunc("/api/catalog", s.handleCatalog)
	s.router.HandleFunc("/api/catalog/", s.handleCatalogApp)

	// Apps (installed)
	s.router.HandleFunc("/api/apps", s.handleApps)
	s.router.HandleFunc("/api/apps/", s.handleAppDetail)

	// Disks
	s.router.HandleFunc("/api/disks", s.handleDisks)
	s.router.HandleFunc("/api/disks/recommend", s.handleDiskRecommend)

	// Volumes
	s.router.HandleFunc("/api/volumes", s.handleVolumes)
	s.router.HandleFunc("/api/volumes/zfs", s.handleZFSPools)

	// Logs & terminal (WebSocket)
	s.router.HandleFunc("/api/ws/logs", s.handleLogsWS)
	s.router.HandleFunc("/api/ws/exec", s.handleExecWS)

	// Shares
	s.router.HandleFunc("/api/shares", s.handleShares)
	s.router.HandleFunc("/api/shares/", s.handleShareDetail)
	s.router.HandleFunc("/api/shares/config/samba", s.handleSambaConfig)
	s.router.HandleFunc("/api/shares/config/nfs", s.handleNFSConfig)

	// Notifications
	s.router.HandleFunc("/api/notifications", s.handleNotifications)
	s.router.HandleFunc("/api/notifications/test", s.handleNotificationTest)

	// Metrics & Dashboard
	s.router.HandleFunc("/api/metrics", s.handleMetrics)
	s.router.HandleFunc("/api/dashboard", s.handleDashboard)

	// Serve UI static files
	s.router.Handle("/", http.FileServer(http.Dir("/var/nasos/ui")))
}

// Start starts the HTTP server.
func (s *Server) Start() error {
	s.server = &http.Server{
		Addr:    s.addr,
		Handler: s.router,
	}
	log.Printf("NasOS API listening on %s", s.addr)
	return s.server.ListenAndServe()
}

// Shutdown gracefully shuts down the server.
func (s *Server) Shutdown(ctx context.Context) error {
	return s.server.Shutdown(ctx)
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

// writeJSON writes a JSON response.
func writeJSON(w http.ResponseWriter, status int, data interface{}) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(data)
}

// writeError writes an error response.
func writeError(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, map[string]string{"error":msg})
}
