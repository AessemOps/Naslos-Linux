// Package apps owns installed-app records and the install/upgrade/uninstall
// orchestration. Records are persisted as JSON beside the other state files and
// carry the exposure settings the routing layer renders from.
package apps

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"sync"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/catalog"
	"github.com/AessemOps/Naslos-Linux/api/internal/chartsrepo"
	"github.com/AessemOps/Naslos-Linux/api/internal/helm"
)

// dns1123Label matches a lowercase RFC 1123 label.
var dns1123Label = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`)

// Exposure is the orthogonal exposure configuration of an installed app.
type Exposure struct {
	Subdomain string `json:"subdomain"`
	TLS       bool   `json:"tls"`
	Auth      bool   `json:"auth"`
	LocalOnly bool   `json:"localOnly"`

	// Service is the in-release Service name the route targets; Port and Scheme
	// describe how to reach it.
	Service string `json:"service,omitempty"`
	Port    int    `json:"port,omitempty"`
	Scheme  string `json:"scheme,omitempty"`
}

// Record is a persisted installed app.
type Record struct {
	Name         string                 `json:"name"`
	Source       string                 `json:"source"`
	Channel      string                 `json:"channel"`
	ChartPath    string                 `json:"chartPath"`
	ChartVersion string                 `json:"chartVersion"`
	Values       map[string]interface{} `json:"values"`
	Exposure     Exposure               `json:"exposure"`
	// BaseDomain is the domain the exposure subdomain hangs off. It also selects
	// the TLS certificate Secret the route references.
	BaseDomain string `json:"baseDomain,omitempty"`

	CreatedAt time.Time `json:"createdAt"`
	UpdatedAt time.Time `json:"updatedAt"`

	// Orphaned marks a release that was found in the cluster but has no
	// matching catalog entry (backfilled from a pre-refactor install).
	Orphaned bool `json:"orphaned,omitempty"`
	// LastError records a non-fatal failure (e.g. routing could not be applied).
	LastError string `json:"lastError,omitempty"`
}

// View is a record enriched with live release status for API responses.
type View struct {
	Record
	Namespace string `json:"namespace"`
	Status    string `json:"status"`
	Chart     string `json:"chart"`
	URL       string `json:"url,omitempty"`
}

// CatalogProvider returns the current catalog snapshot.
type CatalogProvider func() *catalog.Catalog

// Router renders and applies the exposure layer for a record.
type Router interface {
	Apply(ctx context.Context, rec Record, baseDomain string, ssoDomains []string) error
	Delete(ctx context.Context, name string) error
}

// Config bounds the manager's external dependencies.
type Config struct {
	StorePath string
	// Helm is scoped to the apps namespace.
	Helm *helm.Client
	// Charts resolves repository working trees.
	Charts *chartsrepo.Manager
	// Catalog returns the current catalog snapshot (may be swapped on refresh).
	Catalog CatalogProvider
	// Router applies/removes the exposure layer; may be nil (no routing).
	Router Router
	// BaseDomain is the primary (Helm-owned) domain app subdomains hang off.
	BaseDomain string
	// SSODomains are the Authelia-protected domains; auth is only offered there.
	SSODomains []string
}

// Manager coordinates records and lifecycle operations.
type Manager struct {
	store   *Store
	helm    *helm.Client
	charts  *chartsrepo.Manager
	catalog CatalogProvider
	router  Router

	baseDomain string
	ssoDomains []string

	mu sync.Mutex
}

// NewManager creates an app manager and loads persisted records.
func NewManager(cfg Config) (*Manager, error) {
	store := NewStore(cfg.StorePath)
	if err := store.Load(); err != nil {
		return nil, err
	}
	return &Manager{
		store:      store,
		helm:       cfg.Helm,
		charts:     cfg.Charts,
		catalog:    cfg.Catalog,
		router:     cfg.Router,
		baseDomain: cfg.BaseDomain,
		ssoDomains: cfg.SSODomains,
	}, nil
}

// Records returns all records ordered by name.
func (m *Manager) Records() []Record { return m.store.List() }

// Get returns a record by name.
func (m *Manager) Get(name string) (Record, error) { return m.store.Get(name) }

// GetView returns a record with live release status.
func (m *Manager) GetView(ctx context.Context, name string) (View, error) {
	return m.view(ctx, name)
}

// AuthAllowed reports whether Authelia auth may be enabled for a base domain.
func (m *Manager) AuthAllowed(baseDomain string) bool { return m.authAllowed(baseDomain) }

// InstallRequest is the input of Install.
type InstallRequest struct {
	Name       string                 `json:"name"`
	Values     map[string]interface{} `json:"values"`
	Exposure   *Exposure              `json:"exposure,omitempty"`
	BaseDomain string                 `json:"baseDomain,omitempty"`
}

// Install resolves the chart from the catalog, installs it into the apps
// namespace and records the exposure.
func (m *Manager) Install(ctx context.Context, req InstallRequest) (*View, error) {
	if err := validateReleaseName(req.Name); err != nil {
		return nil, err
	}
	c := m.catalog()
	if c == nil {
		return nil, errors.New("catalog is not available")
	}
	app, err := c.Get(req.Name)
	if err != nil {
		return nil, err
	}
	if m.charts == nil {
		return nil, errors.New("chart repositories are not configured")
	}
	chartDir, err := m.resolveChart(ctx, app)
	if err != nil {
		return nil, err
	}

	values := mergeValues(app.DefaultValues, req.Values)
	exposure := exposureFor(app, req.Exposure)

	if _, err := m.helm.InstallDir(ctx, req.Name, chartDir, values); err != nil {
		return nil, err
	}

	now := time.Now().UTC()
	baseDomain := req.BaseDomain
	if baseDomain == "" {
		baseDomain = m.baseDomain
	}
	rec := Record{
		Name:         req.Name,
		Source:       app.Source,
		Channel:      app.Channel,
		ChartPath:    app.ChartPath,
		ChartVersion: app.Version,
		Values:       values,
		Exposure:     exposure,
		BaseDomain:   baseDomain,
		CreatedAt:    now,
		UpdatedAt:    now,
	}
	if err := m.store.Upsert(rec); err != nil {
		return nil, err
	}
	m.applyRoute(ctx, rec, req.BaseDomain)
	view, err := m.view(ctx, rec.Name)
	if err != nil {
		return nil, err
	}
	return &view, nil
}

// Confirmed is unused; confirmation is enforced at the handler boundary.

// Upgrade reconciles an installed app against its chart, replacing values.
func (m *Manager) Upgrade(ctx context.Context, name string, values map[string]interface{}) (*View, error) {
	rec, err := m.store.Get(name)
	if err != nil {
		return nil, err
	}
	if rec.Orphaned {
		return nil, fmt.Errorf("app %q is orphaned and cannot be reconfigured", name)
	}
	c := m.catalog()
	if c == nil {
		return nil, errors.New("catalog is not available")
	}
	app, err := c.Get(name)
	if err != nil {
		return nil, err
	}
	chartDir, err := m.resolveChart(ctx, app)
	if err != nil {
		return nil, err
	}
	merged := mergeValues(rec.Values, values)
	if _, err := m.helm.UpgradeDir(ctx, name, chartDir, merged); err != nil {
		return nil, err
	}
	rec.Values = merged
	rec.ChartVersion = app.Version
	rec.UpdatedAt = time.Now().UTC()
	rec.LastError = ""
	if err := m.store.Upsert(rec); err != nil {
		return nil, err
	}
	view, err := m.view(ctx, name)
	if err != nil {
		return nil, err
	}
	return &view, nil
}

// SetExposure updates the exposure settings and re-renders the route.
func (m *Manager) SetExposure(ctx context.Context, name string, exposure Exposure, baseDomain string) (*View, error) {
	rec, err := m.store.Get(name)
	if err != nil {
		return nil, err
	}
	if err := validateSubdomain(exposure.Subdomain); err != nil {
		return nil, err
	}
	if baseDomain == "" {
		baseDomain = rec.BaseDomain
	}
	if baseDomain == "" {
		baseDomain = m.baseDomain
	}
	if exposure.Auth && !m.authAllowed(baseDomain) {
		return nil, fmt.Errorf("auth requires the domain to be in the SSO domain list")
	}
	rec.Exposure = exposure
	rec.BaseDomain = baseDomain
	rec.UpdatedAt = time.Now().UTC()
	rec.LastError = ""
	if err := m.store.Upsert(rec); err != nil {
		return nil, err
	}
	m.applyRoute(ctx, rec, baseDomain)
	view, err := m.view(ctx, name)
	if err != nil {
		return nil, err
	}
	return &view, nil
}

// Uninstall removes the release, its route and its record.
func (m *Manager) Uninstall(ctx context.Context, name string) error {
	rec, err := m.store.Get(name)
	if err != nil {
		return err
	}
	if !rec.Orphaned {
		if err := m.helm.Uninstall(ctx, name); err != nil {
			return err
		}
	}
	if m.router != nil {
		if err := m.router.Delete(ctx, name); err != nil {
			return err
		}
	}
	return m.store.Delete(name)
}

// List returns all records with live status.
func (m *Manager) List(ctx context.Context) ([]View, error) {
	releases, err := m.helm.List(ctx)
	if err != nil {
		return nil, err
	}
	byName := make(map[string]helm.App, len(releases))
	for _, rel := range releases {
		byName[rel.Name] = rel
	}

	views := make([]View, 0)
	for _, rec := range m.store.List() {
		view := View{
			Record:    rec,
			Namespace: m.helm.Namespace(),
			Chart:     rec.ChartPath,
		}
		if rel, ok := byName[rec.Name]; ok {
			view.Status = rel.Status
			if rel.Chart != "" {
				view.Chart = rel.Chart
			}
		} else {
			view.Status = "missing"
		}
		view.URL = m.urlFor(rec)
		views = append(views, view)
	}
	sort.Slice(views, func(i, j int) bool { return views[i].Name < views[j].Name })
	return views, nil
}

// Backfill creates orphaned records for releases that predate the refactor.
// Releases already represented by a record, and the platform release itself,
// are skipped.
func (m *Manager) Backfill(ctx context.Context, platformRelease string, releases []helm.App) error {
	for _, rel := range releases {
		if rel.Name == platformRelease {
			continue
		}
		if _, err := m.store.Get(rel.Name); err == nil {
			continue
		}
		rec := Record{
			Name:         rel.Name,
			ChartVersion: rel.Version,
			Values:       rel.Values,
			Orphaned:     true,
			CreatedAt:    time.Now().UTC(),
			UpdatedAt:    time.Now().UTC(),
		}
		if err := m.store.Upsert(rec); err != nil {
			return err
		}
	}
	return nil
}

// ReconcileRoutes re-applies routing for every non-orphaned record. Called on
// startup so a routing CR deleted out of band converges again.
func (m *Manager) ReconcileRoutes(ctx context.Context) error {
	if m.router == nil {
		return nil
	}
	var errs []error
	for _, rec := range m.store.List() {
		if rec.Orphaned {
			continue
		}
		baseDomain := rec.BaseDomain
		if baseDomain == "" {
			baseDomain = m.baseDomain
		}
		if err := m.router.Apply(ctx, rec, baseDomain, m.ssoDomains); err != nil {
			errs = append(errs, fmt.Errorf("app %q: %w", rec.Name, err))
		}
	}
	return errors.Join(errs...)
}

// SSODomains returns the configured SSO domains.
func (m *Manager) SSODomains() []string { return append([]string(nil), m.ssoDomains...) }

// BaseDomain returns the primary base domain.
func (m *Manager) BaseDomain() string { return m.baseDomain }

// applyRoute applies the exposure layer, recording a non-fatal failure on the
// record instead of failing the install: the workload is already running.
func (m *Manager) applyRoute(ctx context.Context, rec Record, baseDomain string) {
	if m.router == nil {
		return
	}
	if baseDomain == "" {
		baseDomain = rec.BaseDomain
	}
	if baseDomain == "" {
		baseDomain = m.baseDomain
	}
	if err := m.router.Apply(ctx, rec, baseDomain, m.ssoDomains); err != nil {
		_, _ = m.store.Update(rec.Name, func(r *Record) { r.LastError = err.Error() })
	}
}

// resolveChart ensures the source cache is fresh and returns the local chart dir.
func (m *Manager) resolveChart(ctx context.Context, app *catalog.App) (string, error) {
	if _, err := m.charts.EnsureFresh(ctx, app.Source, app.Channel); err != nil {
		return "", err
	}
	return m.charts.AppDir(app.Source, app.Channel, app.Name)
}

// view reads a record and its live release status.
func (m *Manager) view(ctx context.Context, name string) (View, error) {
	rec, err := m.store.Get(name)
	if err != nil {
		return View{}, err
	}
	view := View{Record: rec, Namespace: m.helm.Namespace(), Chart: rec.ChartPath}
	if rel, err := m.helm.Get(ctx, name); err == nil && rel != nil {
		view.Status = rel.Status
		if rel.Chart != "" {
			view.Chart = rel.Chart
		}
	} else {
		view.Status = "missing"
	}
	view.URL = m.urlFor(rec)
	return view, nil
}

// urlFor builds the app URL from its exposure.
func (m *Manager) urlFor(rec Record) string {
	if rec.Exposure.Subdomain == "" || m.baseDomain == "" {
		return ""
	}
	host := rec.Exposure.Subdomain + "." + m.baseDomain
	if rec.Exposure.TLS {
		return "https://" + host
	}
	return "http://" + host
}

// authAllowed reports whether auth may be enabled for a domain.
func (m *Manager) authAllowed(baseDomain string) bool {
	if baseDomain == "" {
		baseDomain = m.baseDomain
	}
	if baseDomain == "" {
		return false
	}
	for _, domain := range m.ssoDomains {
		if domain == baseDomain {
			return true
		}
	}
	return false
}

// exposureFor derives exposure from the catalog defaults, the declared service
// and the caller's overrides.
func exposureFor(app *catalog.App, override *Exposure) Exposure {
	e := Exposure{
		Subdomain: app.Exposure.Subdomain,
		TLS:       app.Exposure.TLS,
		Auth:      app.Exposure.Auth,
		LocalOnly: app.Exposure.LocalOnly,
	}
	if len(app.Services) > 0 {
		e.Service = app.Services[0].Name
		e.Port = app.Services[0].Port
		e.Scheme = app.Services[0].Scheme
	}
	if override != nil {
		if override.Subdomain != "" {
			e.Subdomain = override.Subdomain
		}
		e.TLS = override.TLS
		e.Auth = override.Auth
		e.LocalOnly = override.LocalOnly
		if override.Service != "" {
			e.Service = override.Service
		}
		if override.Port != 0 {
			e.Port = override.Port
		}
		if override.Scheme != "" {
			e.Scheme = override.Scheme
		}
	}
	if e.Scheme == "" {
		e.Scheme = "http"
	}
	return e
}

// mergeValues deep-merges overlay into base and returns a new map.
func mergeValues(base, overlay map[string]interface{}) map[string]interface{} {
	out := make(map[string]interface{}, len(base)+len(overlay))
	for k, v := range base {
		out[k] = deepCopy(v)
	}
	for k, v := range overlay {
		if existing, ok := out[k].(map[string]interface{}); ok {
			if incoming, ok := v.(map[string]interface{}); ok {
				out[k] = mergeValues(existing, incoming)
				continue
			}
		}
		out[k] = deepCopy(v)
	}
	return out
}

func deepCopy(v interface{}) interface{} {
	switch value := v.(type) {
	case map[string]interface{}:
		dup := make(map[string]interface{}, len(value))
		for k, item := range value {
			dup[k] = deepCopy(item)
		}
		return dup
	case []interface{}:
		dup := make([]interface{}, len(value))
		for i, item := range value {
			dup[i] = deepCopy(item)
		}
		return dup
	default:
		return value
	}
}

func validateReleaseName(name string) error {
	if name == "" {
		return errors.New("app name is required")
	}
	if len(name) > 53 {
		return fmt.Errorf("app name %q must be 53 characters or fewer", name)
	}
	if !dns1123Label.MatchString(name) {
		return fmt.Errorf("app name %q must be a lowercase DNS-1123 label", name)
	}
	return nil
}

func validateSubdomain(subdomain string) error {
	if subdomain == "" {
		return nil
	}
	if !dns1123Label.MatchString(subdomain) {
		return fmt.Errorf("subdomain %q must be a lowercase DNS-1123 label", subdomain)
	}
	return nil
}

// Store persists records as JSON.
type Store struct {
	path    string
	mu      sync.Mutex
	records map[string]Record
}

// NewStore creates a record store.
func NewStore(path string) *Store {
	return &Store{path: path, records: make(map[string]Record)}
}

// Load reads the records file. A missing file is not an error.
func (s *Store) Load() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.path == "" {
		return nil
	}
	data, err := os.ReadFile(s.path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("reading apps file: %w", err)
	}
	var list []Record
	if err := json.Unmarshal(data, &list); err != nil {
		return fmt.Errorf("parsing apps file: %w", err)
	}
	for _, rec := range list {
		if rec.Name == "" {
			continue
		}
		if rec.Values == nil {
			rec.Values = map[string]interface{}{}
		}
		s.records[rec.Name] = rec
	}
	return nil
}

// List returns records ordered by name.
func (s *Store) List() []Record {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]Record, 0, len(s.records))
	for _, rec := range s.records {
		out = append(out, cloneRecord(rec))
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// Get returns one record.
func (s *Store) Get(name string) (Record, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	rec, ok := s.records[name]
	if !ok {
		return Record{}, fmt.Errorf("app %q not found", name)
	}
	return cloneRecord(rec), nil
}

// Upsert stores a record and persists.
func (s *Store) Upsert(rec Record) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.records[rec.Name] = cloneRecord(rec)
	return s.saveLocked()
}

// Update applies a mutation and persists.
func (s *Store) Update(name string, mutate func(*Record)) (Record, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	rec, ok := s.records[name]
	if !ok {
		return Record{}, fmt.Errorf("app %q not found", name)
	}
	mutate(&rec)
	s.records[name] = rec
	return cloneRecord(rec), s.saveLocked()
}

// Delete removes a record.
func (s *Store) Delete(name string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.records[name]; !ok {
		return fmt.Errorf("app %q not found", name)
	}
	delete(s.records, name)
	return s.saveLocked()
}

func (s *Store) saveLocked() error {
	if s.path == "" {
		return nil
	}
	list := make([]Record, 0, len(s.records))
	for _, rec := range s.records {
		list = append(list, rec)
	}
	sort.Slice(list, func(i, j int) bool { return list[i].Name < list[j].Name })
	data, err := json.MarshalIndent(list, "", "  ")
	if err != nil {
		return fmt.Errorf("encoding apps: %w", err)
	}
	if dir := filepath.Dir(s.path); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0750); err != nil {
			return fmt.Errorf("creating apps directory: %w", err)
		}
	}
	tmp, err := os.CreateTemp(filepath.Dir(s.path), ".apps-*.tmp")
	if err != nil {
		return fmt.Errorf("creating temp apps file: %w", err)
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if _, err := tmp.Write(append(data, '\n')); err != nil {
		tmp.Close()
		return fmt.Errorf("writing apps file: %w", err)
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return fmt.Errorf("syncing apps file: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("closing apps file: %w", err)
	}
	if err := os.Chmod(tmpName, 0600); err != nil {
		return fmt.Errorf("setting apps mode: %w", err)
	}
	return os.Rename(tmpName, s.path)
}

func cloneRecord(rec Record) Record {
	dup := rec
	dup.Values = mergeValues(rec.Values, nil)
	return dup
}
