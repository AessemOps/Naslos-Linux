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
	"sync"
	"sync/atomic"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/agent"
	"github.com/AessemOps/Naslos-Linux/api/internal/apps"
	"github.com/AessemOps/Naslos-Linux/api/internal/auth"
	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
	"github.com/AessemOps/Naslos-Linux/api/internal/catalog"
	"github.com/AessemOps/Naslos-Linux/api/internal/certs"
	"github.com/AessemOps/Naslos-Linux/api/internal/chartsrepo"
	"github.com/AessemOps/Naslos-Linux/api/internal/helm"
	"github.com/AessemOps/Naslos-Linux/api/internal/identity"
	"github.com/AessemOps/Naslos-Linux/api/internal/metrics"
	"github.com/AessemOps/Naslos-Linux/api/internal/notifications"
	"github.com/AessemOps/Naslos-Linux/api/internal/routing"
	"github.com/AessemOps/Naslos-Linux/api/internal/shares"
	"github.com/AessemOps/Naslos-Linux/api/internal/talos"
)

// Server is the HTTP API server.
type Server struct {
	addr          string
	talos         *talos.Client
	agent         *agent.Client
	helm          *helm.Client
	catalog       atomic.Pointer[catalog.Catalog]
	shares        *shares.Manager
	sambaUsers    *shares.SambaUserStore
	metrics       *metrics.Manager
	notifications *notifications.Manager
	identity      *identity.Client
	auth          *auth.Middleware
	kubeconfig    string
	// namespace is where Naslos runs; it is the terminal's default namespace and
	// the scope the API's exec permission is limited to.
	namespace string
	// appsNamespace is where user-installed apps run and where the API owns
	// routing and certificates.
	appsNamespace string
	// charts sources/cache and the app record manager.
	charts     *chartsrepo.Manager
	sources    *chartsrepo.Store
	appManager *apps.Manager
	routing    *routing.Reconciler
	certs      *certs.Reconciler
	domains    *certs.Store
	// baseDomain is the primary domain app subdomains hang off; ssoDomains are
	// the Authelia-protected domains.
	baseDomain      string
	ssoDomains      []string
	platformRelease string
	// buddy is the receive side of Buddy Backup: peers push encrypted chunks that
	// this instance stores but cannot read (docs/buddy-backup.md).
	buddy *buddy.Receiver
	// buddyJobs tracks async instance-side sends (POST /api/buddy/send → 202).
	buddyJobs *buddyJobManager
	// buddySchedules persists scheduled backups and drives the runner.
	buddySchedules *buddyScheduleStore
	// schedulerStop stops the backup scheduler; nil until started.
	schedulerStop chan struct{}
	router        *http.ServeMux
	server        *http.Server
	// notificationsMu guards `notifications`. Production sets it once at
	// startup, but job goroutines read it and tests swap it, which was a real
	// data race (CR-06) - so every read goes through notificationManager().
	notificationsMu sync.RWMutex
}

// notificationManager returns the ntfy manager under the read lock.
func (s *Server) notificationManager() *notifications.Manager {
	s.notificationsMu.RLock()
	defer s.notificationsMu.RUnlock()
	return s.notifications
}

// setNotificationManager swaps the ntfy manager under the write lock.
func (s *Server) setNotificationManager(m *notifications.Manager) {
	s.notificationsMu.Lock()
	defer s.notificationsMu.Unlock()
	s.notifications = m
}

// New creates a new server.
func New(addr string, tc *talos.Client) *Server {
	// Initialize share manager. The config path is where share definitions
	// are persisted; without it shares would live only in memory and be lost
	// on every restart (see SHARES_CONFIG / the naslos-shares volume).
	shareManager := shares.NewManager(getEnv("SHARES_CONFIG", "/var/lib/naslos/shares.json"))

	// SMB account mirror: LDAP users' NT hashes, rendered into the passdb
	// import file so SMB logins track LDAP password changes.
	sambaUserStore := shares.NewSambaUserStore(getEnv("SMB_USERS_CONFIG", "/var/lib/naslos/smbusers.json"))

	// Initialize metrics manager
	metricsManager := metrics.NewManager()

	// Initialize notification manager. The settings live on the persistent volume
	// beside the other state files: an empty path means "memory only", which is
	// what this used to be, so every restart silently reset the operator's
	// topic/token/event choices.
	notifManager := notifications.NewManager(getEnv("NOTIFICATIONS_CONFIG", "/var/lib/naslos/notifications.json"))

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

	// Initialize auth middleware. PROXY_SHARED_SECRET is the value Traefik's
	// proxy-identity middleware injects on every request it forwards. There is
	// no opt-out: without it the API refuses to start rather than serve owner
	// routes whose identity header anyone in the trusted CIDR could forge.
	proxySecret := getEnv("PROXY_SHARED_SECRET", "")
	if proxySecret == "" {
		log.Fatalf("PROXY_SHARED_SECRET is required; set it from the naslos-proxy Secret")
	}
	authMiddleware, err := auth.NewMiddleware([]string{
		getEnv("TRAEFIK_CIDR", "10.0.0.0/8"),
	}, proxySecret)
	if err != nil {
		log.Fatalf("Failed to create auth middleware: %v", err)
	}

	// Agent client for ZFS pool operations. The agent DaemonSet is fronted by
	// the headless naslos-agent Service (see agent-daemonset.yaml); the base
	// URL defaults to the in-namespace DNS name and is overridable via
	// AGENT_BASE_URL for local dev (e.g. with a kubectl port-forward).
	namespace := getEnv("NASLOS_NAMESPACE", "naslos")
	appsNamespace := getEnv("APPS_NAMESPACE", "naslos-apps")
	baseDomain := getEnv("NASLOS_DOMAIN", getEnv("DOMAIN", "naslos.local"))
	ssoDomains := getEnvList("SSO_DOMAINS", []string{baseDomain})
	platformRelease := getEnv("PLATFORM_RELEASE", namespace)
	helmClient := helm.NewClient(namespace)
	appsHelmClient := helm.NewClient(appsNamespace)
	agentBaseURL := getEnv("AGENT_BASE_URL", fmt.Sprintf(agent.DefaultBaseURLPattern, namespace))
	// The agent requires the shared token on every request but /health; the same
	// value is mounted into both workloads from the naslos-agent-auth Secret.
	// When the chart serves the agent over TLS it also sets AGENT_CA_FILE, and
	// the certificate is pinned to that CA (PF-M5). A configured-but-broken CA
	// is fatal rather than a silent downgrade to cleartext.
	agentClient, err := agent.NewClientWithCA(agentBaseURL, getEnv("AGENT_TOKEN", ""), getEnv("AGENT_CA_FILE", ""))
	if err != nil {
		log.Fatalf("Agent TLS configuration: %v", err)
	}

	// Buddy Backup, receive side. The peer registry lives with the other state
	// files; the chunks live on the backup dataset (BUDDY_RECEIVE_PATH), which is
	// mounted read-write into the API precisely because this is the one place a
	// non-owner may write, and it can only ever write opaque ciphertext.
	buddyPeers := buddy.NewPeerStore(getEnv("BUDDY_PEERS", "/var/lib/naslos/buddy-peers.json"))
	if err := buddyPeers.Load(); err != nil {
		log.Printf("Warning: could not load the buddy peer registry: %v", err)
	}
	buddyStorePath := getEnv("BUDDY_RECEIVE_PATH", "/var/lib/naslos/buddy")
	buddyAuth := buddy.NewAuthenticator(buddyPeers)
	// Keep the replay cache across restarts: a request captured just before a
	// restart could otherwise be replayed inside the clock-skew window (NAS-014).
	buddyAuth.PersistNonces(buddyStorePath)

	buddyReceiver := &buddy.Receiver{
		Store:       buddy.NewStore(buddyStorePath),
		Peers:       buddyPeers,
		Auth:        buddyAuth,
		Name:        getEnv("BUDDY_NAME", "naslos"),
		Version:     getEnv("BUDDY_VERSION", "1"),
		EnrollToken: getEnv("BUDDY_ENROLL_TOKEN", ""),
	}

	s := &Server{
		addr:            addr,
		talos:           tc,
		agent:           agentClient,
		helm:            helmClient,
		shares:          shareManager,
		sambaUsers:      sambaUserStore,
		metrics:         metricsManager,
		notifications:   notifManager,
		identity:        identityClient,
		auth:            authMiddleware,
		namespace:       namespace,
		appsNamespace:   appsNamespace,
		baseDomain:      baseDomain,
		ssoDomains:      ssoDomains,
		platformRelease: platformRelease,
		buddy:           buddyReceiver,
		// Owner routes are gated on the proxy secret by the composed router in
		// routes().
		router: http.NewServeMux(),
	}
	s.setupChartRepos(appsHelmClient)
	s.routes()
	return s
}

// setupChartRepos wires the source store, the git cache, the catalog snapshot,
// the app records manager and the routing/certificates reconcilers.
func (s *Server) setupChartRepos(appsHelmClient *helm.Client) {
	sources := chartsrepo.NewStore(getEnv("SOURCES_CONFIG", "/var/lib/naslos/sources.json"))
	if err := sources.Load(); err != nil {
		log.Printf("Warning: could not load chart sources: %v", err)
	}
	s.sources = sources

	creds := serverCredentials{s: s, defaultNS: s.namespace}
	s.charts = chartsrepo.NewManager(
		sources,
		getEnv("CHARTS_CACHE_DIR", "/var/lib/naslos/charts"),
		getEnvDuration("CHARTS_TTL", chartsrepo.DefaultTTL),
		creds,
	)

	// Seed the official source once, from the chart's environment.
	if url := getEnv("SOURCES_OFFICIAL_URL", ""); url != "" {
		if _, err := sources.Get(getEnv("SOURCES_OFFICIAL_NAME", "naslos")); err != nil {
			official := &chartsrepo.Source{
				Name:              getEnv("SOURCES_OFFICIAL_NAME", "naslos"),
				DisplayName:       getEnv("SOURCES_OFFICIAL_DISPLAY", "NaslosCharts"),
				URL:               url,
				Auth:              chartsrepo.AuthType(getEnv("SOURCES_OFFICIAL_AUTH", string(chartsrepo.AuthPublic))),
				CredentialsSecret: getEnv("SOURCES_OFFICIAL_SECRET", ""),
				Official:          true,
			}
			if _, err := s.charts.AddSource(official); err != nil {
				log.Printf("Warning: could not seed the official chart source: %v", err)
			}
		}
	}

	s.catalog.Store(s.buildCatalog())

	// Base domains are loaded before the routing reconciler, whose TLS-secret
	// resolver reads them.
	domains := certs.NewStore(getEnv("DOMAINS_CONFIG", "/var/lib/naslos/domains.json"))
	if err := domains.Load(); err != nil {
		log.Printf("Warning: could not load domains: %v", err)
	}
	s.domains = domains

	// Dynamic client for routing and certificates. A failure here (e.g. no
	// cluster in a unit test) degrades to no routing rather than a fatal error.
	var router apps.Router
	if dyn, err := s.dynamicKubernetesClient(); err == nil {
		s.routing = routing.NewReconciler(dyn, routing.Options{
			Namespace:         s.appsNamespace,
			TLSSecret:         getEnv("APPS_TLS_SECRET", "naslos-apps-tls"),
			AutheliaService:   getEnv("AUTHELIA_SERVICE", "naslos-authelia"),
			AutheliaPort:      getEnvInt("AUTHELIA_PORT", 80),
			AutheliaNamespace: s.namespace,
			LocalOnlyCIDR:     getEnv("EXPOSURE_LOCAL_ONLY_CIDR", ""),
			// A domain's wildcard Certificate writes a per-domain Secret; the
			// route must reference that, not a fixed name.
			TLSSecretFor: func(baseDomain string) string {
				if s.domains == nil {
					return ""
				}
				if domain, err := s.domains.Get(baseDomain); err == nil {
					return domain.SecretName()
				}
				return ""
			},
		})
		s.certs = certs.NewReconciler(dyn, s.appsNamespace)
		router = s.routing
	} else {
		log.Printf("Warning: no dynamic client for app routing: %v", err)
	}

	manager, err := apps.NewManager(apps.Config{
		StorePath:  getEnv("APPS_CONFIG", "/var/lib/naslos/apps.json"),
		Helm:       appsHelmClient,
		Charts:     s.charts,
		Catalog:    func() *catalog.Catalog { return s.catalog.Load() },
		Router:     router,
		Discoverer: appServiceDiscoverer{client: s.kubernetesClient},
		BaseDomain: s.baseDomain,
		SSODomains: s.ssoDomains,
	})
	if err != nil {
		log.Printf("Warning: could not load installed-app records: %v", err)
	}
	s.appManager = manager
}

// buildCatalog scans the cached repositories into a catalog snapshot.
func (s *Server) buildCatalog() *catalog.Catalog {
	if s.charts == nil {
		return catalog.Load(nil)
	}
	refs := make([]catalog.SourceRef, 0)
	for _, src := range s.charts.Sources() {
		for _, channel := range src.ChannelNames() {
			dir, err := s.charts.Dir(src.Name, channel)
			if err != nil {
				continue
			}
			refs = append(refs, catalog.SourceRef{
				Name:        src.Name,
				DisplayName: src.DisplayName,
				Channel:     channel,
				Dir:         dir,
				Official:    src.Official,
			})
		}
	}
	return catalog.Load(refs)
}

// reconcileApps refreshes sources, rebuilds the catalog, backfills records and
// converges routing. It is best-effort: failures are logged, not fatal.
func (s *Server) reconcileApps() {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()

	if err := s.refreshSources(ctx); err != nil {
		log.Printf("Warning: chart repository refresh failed: %v", err)
	}
	if s.appManager == nil {
		return
	}
	if releases, err := s.discoverPlatformReleases(ctx); err == nil {
		if err := s.appManager.Backfill(ctx, s.platformRelease, releases); err != nil {
			log.Printf("Warning: app backfill failed: %v", err)
		}
	} else {
		log.Printf("Warning: could not discover platform releases for backfill: %v", err)
	}
	if err := s.appManager.ReconcileRoutes(ctx); err != nil {
		log.Printf("Warning: app route reconcile failed: %v", err)
	}
}

// refreshSources refreshes every configured source and rebuilds the snapshot.
func (s *Server) refreshSources(ctx context.Context) error {
	if s.charts == nil {
		return nil
	}
	err := s.charts.RefreshAll(ctx)
	s.catalog.Store(s.buildCatalog())
	return err
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

// getEnvDuration returns a duration environment variable or a default.
func getEnvDuration(key string, defaultValue time.Duration) time.Duration {
	if value, ok := os.LookupEnv(key); ok {
		if parsed, err := time.ParseDuration(value); err == nil {
			return parsed
		}
	}
	return defaultValue
}

// routes registers all API routes.
//
// Every owner-facing route lives on the `owner` mux, which is wrapped in the
// auth middleware (proxy secret + trusted source + identity header). Only the
// health probes, the buddy
// peer API (authenticated by the peers' own Ed25519 keys) and the static UI are
// public; ServeMux's longest-pattern match makes those beat the "/api/"
// catch-all, so a new owner endpoint cannot be added unauthenticated by
// accident - it goes on `owner`.
func (s *Server) routes() {
	owner := http.NewServeMux()

	// Health: public so liveness/readiness probes keep working even when
	// authentication is misconfigured.
	s.router.HandleFunc("/api/health", s.handleHealth)
	s.router.HandleFunc("/api/ready", s.handleReady)

	// Catalog (app store)
	owner.HandleFunc("/api/catalog", s.handleCatalog)
	owner.HandleFunc("/api/catalog/", s.handleCatalogApp)

	// Apps (installed)
	owner.HandleFunc("/api/apps", s.handleApps)
	owner.HandleFunc("/api/apps/", s.handleAppDetail)

	// Chart repositories (sources)
	owner.HandleFunc("/api/sources", s.handleSources)
	owner.HandleFunc("/api/sources/refresh", s.handleSourcesRefresh)
	owner.HandleFunc("/api/sources/", s.handleSourceDetail)

	// Domains & certificates
	owner.HandleFunc("/api/domains", s.handleDomains)
	owner.HandleFunc("/api/domains/", s.handleDomainDetail)

	// Disks
	owner.HandleFunc("/api/disks", s.handleDisks)
	owner.HandleFunc("/api/disks/recommend", s.handleDiskRecommend)

	// Volumes. There is deliberately no bare /api/volumes route: it was a stub
	// that answered 201 without applying anything (PF-M8). UserVolumeConfig
	// documents are not wired to a Talos apply path yet.
	owner.HandleFunc("/api/volumes/zfs", s.handleZFSPools)
	owner.HandleFunc("/api/volumes/zfs/import", s.handleZFSImport)
	owner.HandleFunc("/api/volumes/zfs/", s.handleZFSPoolDetail)
	owner.HandleFunc("/api/datasets", s.handleDatasets)

	// Logs & terminal (WebSocket)
	owner.HandleFunc("/api/ws/logs", s.handleLogsWS)
	owner.HandleFunc("/api/pods", s.handlePods)
	owner.HandleFunc("/api/namespaces", s.handleNamespaces)
	owner.HandleFunc("/api/ws/exec", s.handleExecWS)

	// Shares
	owner.HandleFunc("/api/shares", s.handleShares)
	owner.HandleFunc("/api/shares/paths", s.handleSharePaths)
	owner.HandleFunc("/api/shares/folders", s.handleShareFolders)
	owner.HandleFunc("/api/shares/status", s.handleSharesStatus)
	owner.HandleFunc("/api/shares/apply", s.handleSharesApply)
	owner.HandleFunc("/api/shares/config/samba", s.handleSambaConfig)
	owner.HandleFunc("/api/shares/config/nfs", s.handleNFSConfig)
	owner.HandleFunc("/api/shares/", s.handleShareDetail)

	// Notifications
	owner.HandleFunc("/api/notifications", s.handleNotifications)
	owner.HandleFunc("/api/notifications/test", s.handleNotificationTest)

	// Buddy Backup. The peer-facing API is authenticated by the peers' own keys
	// (a peer cannot complete an interactive login), so it stays public; every
	// owner-facing endpoint is on the owner mux like any other owner route.
	if s.buddy != nil {
		s.router.Handle(buddy.PathPrefix+"/", s.buddy.Handler())
		owner.HandleFunc("/api/buddy/status", s.handleBuddyStatus)
		owner.HandleFunc("/api/buddy/peers", s.handleBuddyPeers)
		// Sender side: this instance backing *itself* (and its datasets) up to a
		// buddy, using the agent's streaming `zfs send`/`zfs receive`.
		owner.HandleFunc("/api/buddy/identity", s.handleBuddyIdentity)
		owner.HandleFunc("/api/buddy/send", s.handleBuddySend)
		owner.HandleFunc("/api/buddy/restore", s.handleBuddyRestore)
		owner.HandleFunc("/api/buddy/jobs", s.handleBuddyJobs)
		owner.HandleFunc("/api/buddy/jobs/", s.handleBuddyJobDetail)
		owner.HandleFunc("/api/buddy/schedules", s.handleBuddySchedules)
	}

	// Metrics & Dashboard are registered below on the outer router: any
	// authenticated user may see them (see the allow-list comment there).

	// Users & Groups (identity management)
	owner.HandleFunc("/api/users", s.handleUsers)
	owner.HandleFunc("/api/users/", s.handleUserPath)
	owner.HandleFunc("/api/groups", s.handleGroups)
	owner.HandleFunc("/api/groups/", s.handleGroupDetail)

	// What any *authenticated* user may reach: their own identity and the
	// read-only dashboard. Everything else is administrator-only (CR-03).
	//
	// The allow-list is deliberately explicit and small: the owner mux below is
	// mounted behind RequireAdmin, so a route added there is admin by default and
	// cannot be exposed by accident.
	s.router.Handle("/api/auth/me", s.requireOwnerAuth(http.HandlerFunc(s.handleAuthMe)))
	s.router.Handle("/api/dashboard", s.requireOwnerAuth(http.HandlerFunc(s.handleDashboard)))
	s.router.Handle("/api/metrics", s.requireOwnerAuth(http.HandlerFunc(s.handleMetrics)))

	// Everything else under /api/ requires an authenticated *administrator*.
	// `naslos_admins` is the group Authelia forwards; without this gate any
	// authenticated account could manage users, wipe disks or open the root
	// terminal (CR-03). There is no bypass.
	s.router.Handle("/api/", s.requireOwnerAuth(s.requireAdmin(owner)))

	// Serve UI static files
	s.router.Handle("/", http.FileServer(http.Dir("/var/naslos/ui")))
}

// requireOwnerAuth wraps the owner route mux in the authentication middleware.
// There is no opt-out: a request without the proxy secret is always rejected.
func (s *Server) requireOwnerAuth(next http.Handler) http.Handler {
	return s.auth.RequireAuth(next)
}

// requireAdmin additionally requires the authenticated user to be a Naslos
// administrator (the `naslos_admins` group). It runs after RequireAuth, so the
// user is already in the request context.
func (s *Server) requireAdmin(next http.Handler) http.Handler {
	return s.auth.RequireAdmin(next)
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

	// Refresh the chart repositories, rebuild the catalog, backfill records and
	// converge routing. Runs in the background so an unreachable remote cannot
	// block the API from serving.
	go s.reconcileApps()

	// Async buddy sends and the backup scheduler (FR-BUD-15/16).
	s.ensureBuddyJobs()
	s.ensureBuddySchedules()
	s.startBuddyScheduler()

	// ZFS health and disk-presence notifications (PF-M9). No-op unless the
	// operator enabled the matching events.
	s.startHealthNotifier()

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
		// Bound slow clients (gosec G112 / Slowloris): without
		// ReadHeaderTimeout one connection can hold a worker open indefinitely.
		// WriteTimeout stays 0 on purpose - the terminal and log streams are
		// long-lived websockets that a write deadline would cut.
		ReadHeaderTimeout: 10 * time.Second,
		ReadTimeout:       30 * time.Second,
		IdleTimeout:       120 * time.Second,
	}
	log.Printf("Naslos API listening on %s", s.addr)
	return s.server.ListenAndServe()
}

// Shutdown gracefully shuts down the server.
func (s *Server) Shutdown(ctx context.Context) error {
	s.stopBuddyScheduler()
	if s.buddyJobs != nil {
		s.buddyJobs.cancelAll()
	}
	if s.server == nil {
		return nil
	}
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
