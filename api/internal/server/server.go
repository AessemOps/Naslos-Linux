// Package server provides the Naslos HTTP API server.
package server

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"os"
	"strconv"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/agent"
	"github.com/AessemOps/Naslos-Linux/api/internal/auth"
	"github.com/AessemOps/Naslos-Linux/api/internal/catalog"
	"github.com/AessemOps/Naslos-Linux/api/internal/helm"
	"github.com/AessemOps/Naslos-Linux/api/internal/identity"
	"github.com/AessemOps/Naslos-Linux/api/internal/metrics"
	"github.com/AessemOps/Naslos-Linux/api/internal/notifications"
	"github.com/AessemOps/Naslos-Linux/api/internal/shares"
	"github.com/AessemOps/Naslos-Linux/api/internal/talos"
)

// Server is the HTTP API server.
type Server struct {
	addr          string
	talos         *talos.Client
	agent         *agent.Client
	helm          *helm.Client
	catalog       *catalog.Catalog
	shares        *shares.Manager
	sambaUsers    *shares.SambaUserStore
	metrics       *metrics.Manager
	notifications *notifications.Manager
	identity      *identity.Client
	auth          *auth.Middleware
	kubeconfig    string
	router        *http.ServeMux
	server        *http.Server
}

// New creates a new server.
func New(addr string, tc *talos.Client) *Server {
	// Initialize Helm client for the naslos namespace
	helmClient := helm.NewClient("naslos")

	// Initialize app catalog (built-in)
	c := catalog.New("")

	// Initialize share manager. The config path is where share definitions
	// are persisted; without it shares would live only in memory and be lost
	// on every restart (see SHARES_CONFIG / the naslos-shares volume).
	shareManager := shares.NewManager(getEnv("SHARES_CONFIG", "/var/lib/naslos/shares.json"))

	// SMB account mirror: LDAP users' NT hashes, rendered into the passdb
	// import file so SMB logins track LDAP password changes.
	sambaUserStore := shares.NewSambaUserStore(getEnv("SMB_USERS_CONFIG", "/var/lib/naslos/smbusers.json"))

	// Initialize metrics manager
	metricsManager := metrics.NewManager()

	// Initialize notification manager
	notifManager := notifications.NewManager("")

	// Initialize identity client (LDAP)
	identityClient, err := identity.NewClient(identity.Config{
		Host:       getEnv("LDAP_HOST", "naslos-openldap"),
		Port:       getEnvInt("LDAP_PORT", 636),
		BaseDN:     getEnv("LDAP_BASE_DN", "dc=naslos,dc=local"),
		BindDN:     getEnv("LDAP_BIND_DN", "cn=naslos-service,ou=services,dc=naslos,dc=local"),
		BindPass:   getEnv("LDAP_BIND_PASS", ""),
		UseTLS:     getEnv("LDAP_USE_TLS", "true") == "true",
		CACertPath: getEnv("LDAP_CA_CERT", ""),
	})
	if err != nil {
		log.Printf("Warning: Failed to connect to LDAP: %v", err)
		// Continue without identity - will retry on first use
		identityClient = nil
	}

	// Initialize auth middleware
	authMiddleware, err := auth.NewMiddleware([]string{
		getEnv("TRAEFIK_CIDR", "10.0.0.0/8"),
	})
	if err != nil {
		log.Fatalf("Failed to create auth middleware: %v", err)
	}

	// Agent client for ZFS pool operations. The agent DaemonSet is fronted by
	// the headless naslos-agent Service (see agent-daemonset.yaml); the base
	// URL defaults to the in-namespace DNS name and is overridable via
	// AGENT_BASE_URL for local dev (e.g. with a kubectl port-forward).
	namespace := getEnv("NASLOS_NAMESPACE", "naslos")
	agentBaseURL := getEnv("AGENT_BASE_URL", fmt.Sprintf(agent.DefaultBaseURLPattern, namespace))
	agentClient := agent.NewClient(agentBaseURL)

	s := &Server{
		addr:          addr,
		talos:         tc,
		agent:         agentClient,
		helm:          helmClient,
		catalog:       c,
		shares:        shareManager,
		sambaUsers:    sambaUserStore,
		metrics:       metricsManager,
		notifications: notifManager,
		identity:      identityClient,
		auth:          authMiddleware,
		router:        http.NewServeMux(),
	}
	s.routes()
	return s
}

// getEnv returns the value of an environment variable or a default.
func getEnv(key, defaultValue string) string {
	if value, ok := os.LookupEnv(key); ok {
		return value
	}
	return defaultValue
}

// getEnvInt returns the integer value of an environment variable or a default.
func getEnvInt(key string, defaultValue int) int {
	if value, ok := os.LookupEnv(key); ok {
		if intVal, err := strconv.Atoi(value); err == nil {
			return intVal
		}
	}
	return defaultValue
}

// routes registers all API routes.
func (s *Server) routes() {
	// Health
	s.router.HandleFunc("/api/health", s.handleHealth)
	s.router.HandleFunc("/api/ready", s.handleReady)

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
	s.router.HandleFunc("/api/volumes/zfs/import", s.handleZFSImport)
	s.router.HandleFunc("/api/volumes/zfs/", s.handleZFSPoolDetail)

	// Logs & terminal (WebSocket)
	s.router.HandleFunc("/api/ws/logs", s.handleLogsWS)
	s.router.HandleFunc("/api/ws/exec", s.handleExecWS)

	// Shares
	s.router.HandleFunc("/api/shares", s.handleShares)
	s.router.HandleFunc("/api/shares/paths", s.handleSharePaths)
	s.router.HandleFunc("/api/shares/folders", s.handleShareFolders)
	s.router.HandleFunc("/api/shares/status", s.handleSharesStatus)
	s.router.HandleFunc("/api/shares/apply", s.handleSharesApply)
	s.router.HandleFunc("/api/shares/config/samba", s.handleSambaConfig)
	s.router.HandleFunc("/api/shares/config/nfs", s.handleNFSConfig)
	s.router.HandleFunc("/api/shares/", s.handleShareDetail)

	// Notifications
	s.router.HandleFunc("/api/notifications", s.handleNotifications)
	s.router.HandleFunc("/api/notifications/test", s.handleNotificationTest)

	// Metrics & Dashboard
	s.router.HandleFunc("/api/metrics", s.handleMetrics)
	s.router.HandleFunc("/api/dashboard", s.handleDashboard)

	// Users & Groups (identity management)
	s.router.HandleFunc("/api/users", s.handleUsers)
	s.router.HandleFunc("/api/users/", s.handleUserPath)
	s.router.HandleFunc("/api/groups", s.handleGroups)
	s.router.HandleFunc("/api/groups/", s.handleGroupDetail)
	s.router.HandleFunc("/api/auth/me", s.handleAuthMe)

	// Serve UI static files
	s.router.Handle("/", http.FileServer(http.Dir("/var/naslos/ui")))
}

// handleUserPath routes user sub-paths (password, enable, disable).
func (s *Server) handleUserPath(w http.ResponseWriter, r *http.Request) {
	path := r.URL.Path[len("/api/users/"):]

	// Check for sub-paths
	if strings.HasSuffix(path, "/password") {
		s.handleUserPassword(w, r)
		return
	}
	if strings.HasSuffix(path, "/enable") || strings.HasSuffix(path, "/disable") {
		s.handleUserEnable(w, r)
		return
	}

	// Default: user detail
	s.handleUserDetail(w, r)
}

// Start starts the HTTP server.
func (s *Server) Start() error {
	// Kick off the background metrics collector so the dashboard has data
	// as soon as the server comes up.
	s.startMetricsCollector()

	// Converge the node's share services with the persisted share definitions.
	// This covers first boot, chart upgrades and node reboots: the host's
	// config directory may be empty or stale, and the API is the source of
	// truth. Failures are logged only — the API stays useful without shares.
	go func() {
		if _, err := s.applySharesConfig(); err != nil {
			log.Printf("Warning: initial shares apply failed: %v", err)
		}
	}()

	s.server = &http.Server{
		Addr:    s.addr,
		Handler: s.router,
	}
	log.Printf("Naslos API listening on %s", s.addr)
	return s.server.ListenAndServe()
}

// Shutdown gracefully shuts down the server.
func (s *Server) Shutdown(ctx context.Context) error {
	return s.server.Shutdown(ctx)
}

// identityUnavailable reports (and writes) an error when LDAP cannot be used.
// The check actively attempts to (re)connect, so a client that could not reach
// LDAP at startup — or lost it later — recovers on its own instead of failing
// permanently until the API pod is restarted.
func (s *Server) identityUnavailable(w http.ResponseWriter) bool {
	if s.identity == nil {
		writeError(w, http.StatusServiceUnavailable,
			"Identity/LDAP is not configured (check LDAP_HOST, LDAP_BIND_PASS, and that OpenLDAP is running)")
		return true
	}
	if err := s.identity.EnsureConnection(); err != nil {
		log.Printf("LDAP unavailable: %v", err)
		writeError(w, http.StatusServiceUnavailable,
			fmt.Sprintf("Identity/LDAP is not reachable (will retry automatically): %v", err))
		return true
	}
	return false
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

// handleReady reports process readiness. It intentionally does NOT fail when
// LDAP is down: the UI and the dashboard remain usable, and the identity
// client retries on demand. The LDAP state is reported for observability.
func (s *Server) handleReady(w http.ResponseWriter, r *http.Request) {
	ldapState := "down"
	if s.identity != nil && s.identity.EnsureConnection() == nil {
		ldapState = "up"
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok", "ldap": ldapState})
}

// writeJSON writes a JSON response.
func writeJSON(w http.ResponseWriter, status int, data interface{}) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(data)
}

// writeError writes an error response.
func writeError(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, map[string]string{"error": msg})
}
