// Package ddns implements Dynamic DNS: it keeps A/AAAA records pointed at the
// appliance's public IP. Entries are persisted as JSON; a background manager
// detects the public IP on an interval and drives the provider's DDNS driver.
// Credentials live in Kubernetes Secrets in the apps namespace and are never
// returned by the API.
package ddns

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"

	"github.com/AessemOps/Naslos-Linux/api/internal/providers"
)

// Entry is one dynamic-DNS record. It never holds credential values: secret
// fields live in CredentialsSecret and only the set field names are recorded.
type Entry struct {
	ID         string `json:"id"`
	Provider   string `json:"provider"`
	Zone       string `json:"zone"`
	Record     string `json:"record"`
	RecordType string `json:"recordType"`
	TTL        int    `json:"ttl,omitempty"`
	Enabled    bool   `json:"enabled"`

	// ProviderConfig holds the provider's non-secret fields.
	ProviderConfig map[string]string `json:"providerConfig,omitempty"`
	// CredentialsSecret is the Kubernetes Secret holding the secret fields.
	CredentialsSecret string `json:"credentialsSecret,omitempty"`
	// CredentialFields names which secret fields are set (never their values).
	CredentialFields []string `json:"credentialFields,omitempty"`

	LastIP     string    `json:"lastIP,omitempty"`
	LastStatus string    `json:"lastStatus,omitempty"`
	LastError  string    `json:"lastError,omitempty"`
	LastRunAt  time.Time `json:"lastRunAt,omitempty"`
	NextRunAt  time.Time `json:"nextRunAt,omitempty"`

	CreatedAt time.Time `json:"createdAt"`
	UpdatedAt time.Time `json:"updatedAt"`
}

// MarshalJSON omits timestamps that were never set (omitempty does nothing for
// time.Time, which would otherwise serialise the Go zero time).
func (e *Entry) MarshalJSON() ([]byte, error) {
	type entry Entry
	out := struct {
		*entry
		LastRunAt *time.Time `json:"lastRunAt,omitempty"`
		NextRunAt *time.Time `json:"nextRunAt,omitempty"`
	}{entry: (*entry)(e)}
	if !e.LastRunAt.IsZero() {
		out.LastRunAt = &e.LastRunAt
	}
	if !e.NextRunAt.IsZero() {
		out.NextRunAt = &e.NextRunAt
	}
	return json.Marshal(out)
}

// SecretName is the Secret the entry's credential fields live in.
func (e *Entry) SecretName() string {
	return "naslos-ddns-" + e.ID
}

// NewID returns a random DNS-1123-safe entry id.
func NewID() string {
	buf := make([]byte, 8)
	if _, err := rand.Read(buf); err != nil {
		return fmt.Sprintf("ddns-%d", time.Now().UnixNano())
	}
	return hex.EncodeToString(buf)
}

// Store persists entries as JSON with atomic writes, modeled on certs.Store.
type Store struct {
	path string

	mu      sync.Mutex
	entries map[string]Entry
}

// NewStore creates an entry store.
func NewStore(path string) *Store {
	return &Store{path: path, entries: make(map[string]Entry)}
}

// Load reads the entries file.
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
		return fmt.Errorf("reading ddns file: %w", err)
	}
	var list []Entry
	if err := json.Unmarshal(data, &list); err != nil {
		return fmt.Errorf("parsing ddns file: %w", err)
	}
	for _, e := range list {
		if e.ID == "" {
			continue
		}
		s.entries[e.ID] = e
	}
	return nil
}

// List returns entries ordered by id.
func (s *Store) List() []Entry {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]Entry, 0, len(s.entries))
	for _, e := range s.entries {
		out = append(out, e)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out
}

// Get returns an entry.
func (s *Store) Get(id string) (Entry, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, ok := s.entries[id]
	if !ok {
		return Entry{}, fmt.Errorf("ddns entry %q not found", id)
	}
	return e, nil
}

// Put stores an entry.
func (s *Store) Put(e Entry) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.entries[e.ID] = e
	return s.saveLocked()
}

// Delete removes an entry.
func (s *Store) Delete(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.entries[id]; !ok {
		return fmt.Errorf("ddns entry %q not found", id)
	}
	delete(s.entries, id)
	return s.saveLocked()
}

// Update applies a mutation and persists.
func (s *Store) Update(id string, mutate func(*Entry)) (Entry, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, ok := s.entries[id]
	if !ok {
		return Entry{}, fmt.Errorf("ddns entry %q not found", id)
	}
	mutate(&e)
	s.entries[id] = e
	return e, s.saveLocked()
}

func (s *Store) saveLocked() error {
	if s.path == "" {
		return nil
	}
	list := make([]Entry, 0, len(s.entries))
	for _, e := range s.entries {
		list = append(list, e)
	}
	sort.Slice(list, func(i, j int) bool { return list[i].ID < list[j].ID })
	data, err := json.MarshalIndent(list, "", "  ")
	if err != nil {
		return fmt.Errorf("encoding ddns entries: %w", err)
	}
	if dir := filepath.Dir(s.path); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0750); err != nil {
			return err
		}
	}
	tmp, err := os.CreateTemp(filepath.Dir(s.path), ".ddns-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if _, err := tmp.Write(append(data, '\n')); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	if err := os.Chmod(tmpName, 0600); err != nil {
		return err
	}
	return os.Rename(tmpName, s.path)
}

// DetectIP fetches a public IP from a plain-text IP source URL and validates it.
func DetectIP(ctx context.Context, client *http.Client, source string) (string, error) {
	source = strings.TrimSpace(source)
	if source == "" {
		return "", fmt.Errorf("no public-IP source is configured")
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, source, nil)
	if err != nil {
		return "", fmt.Errorf("invalid IP source: %w", err)
	}
	req.Header.Set("User-Agent", UserAgent)
	if client == nil {
		client = DefaultClient()
	}
	resp, err := client.Do(req)
	if err != nil {
		return "", fmt.Errorf("detecting the public IP: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("IP source %s returned HTTP %d", source, resp.StatusCode)
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, 128))
	if err != nil {
		return "", fmt.Errorf("reading the IP source response: %w", err)
	}
	ip := net.ParseIP(strings.TrimSpace(string(body)))
	if ip == nil {
		return "", fmt.Errorf("IP source %s returned %q, not an IP", source, strings.TrimSpace(string(body)))
	}
	return ip.String(), nil
}

// WriteSecret creates or updates a credential Secret. On update the submitted
// values are merged over the existing keys, so a partial update (one field left
// blank in the form) never drops the other stored credentials.
func WriteSecret(ctx context.Context, client kubernetes.Interface, namespace, name string, data map[string]string) error {
	if client == nil {
		return fmt.Errorf("no Kubernetes client for credentials")
	}
	if name == "" {
		return fmt.Errorf("no Secret name")
	}
	encoded := make(map[string][]byte, len(data))
	for k, v := range data {
		encoded[k] = []byte(v)
	}
	secrets := client.CoreV1().Secrets(namespace)
	existing, err := secrets.Get(ctx, name, metav1.GetOptions{})
	if apierrors.IsNotFound(err) {
		_, err = secrets.Create(ctx, corev1Secret(name, encoded), metav1.CreateOptions{})
		return err
	}
	if err != nil {
		return err
	}
	merged := make(map[string][]byte, len(existing.Data)+len(encoded))
	for k, v := range existing.Data {
		merged[k] = v
	}
	for k, v := range encoded {
		merged[k] = v
	}
	existing.Data = merged
	_, err = secrets.Update(ctx, existing, metav1.UpdateOptions{})
	return err
}

// DeleteSecret removes a credential Secret, ignoring a missing one.
func DeleteSecret(ctx context.Context, client kubernetes.Interface, namespace, name string) error {
	if client == nil || name == "" {
		return nil
	}
	err := client.CoreV1().Secrets(namespace).Delete(ctx, name, metav1.DeleteOptions{})
	if err != nil && !apierrors.IsNotFound(err) {
		return err
	}
	return nil
}

// Options configures a Manager.
type Options struct {
	Registry      *providers.Registry
	StorePath     string
	AppsNamespace string
	// Clientset resolves the Kubernetes client lazily; a failure is reported per
	// entry rather than blocking startup.
	Clientset  func() (kubernetes.Interface, error)
	IPSource   string
	IPv6Source string
	Interval   time.Duration
	Client     *http.Client
}

// Manager detects the public IP and keeps entries converged.
type Manager struct {
	registry      *providers.Registry
	store         *Store
	appsNamespace string
	clientset     func() (kubernetes.Interface, error)
	ipSource      string
	ipv6Source    string
	interval      time.Duration
	client        *http.Client
	now           func() time.Time

	// newDriver is a test seam; production uses defaultDriver.
	newDriver func(driver string, client *http.Client) (Driver, error)

	lastLoadErr error

	mu   sync.Mutex
	stop chan struct{}
}

// NewManager creates a Manager and loads its store.
func NewManager(opts Options) *Manager {
	interval := opts.Interval
	if interval <= 0 {
		interval = 5 * time.Minute
	}
	client := opts.Client
	if client == nil {
		client = DefaultClient()
	}
	m := &Manager{
		registry:      opts.Registry,
		store:         NewStore(opts.StorePath),
		appsNamespace: opts.AppsNamespace,
		clientset:     opts.Clientset,
		ipSource:      opts.IPSource,
		ipv6Source:    opts.IPv6Source,
		interval:      interval,
		client:        client,
		now:           func() time.Time { return time.Now().UTC() },
	}
	m.newDriver = defaultDriver
	if err := m.store.Load(); err != nil {
		// A corrupt file must not stop the API from serving; it is surfaced
		// through the errors below on first use.
		m.lastLoadErr = err
	}
	return m
}

// lastLoadErr is reported by LoadError.
func (m *Manager) LoadError() error { return m.lastLoadErr }

// Store returns the entry store.
func (m *Manager) Store() *Store { return m.store }

// Interval returns the reconcile interval.
func (m *Manager) Interval() time.Duration { return m.interval }

// Start launches the reconcile loop (immediately, then on each tick).
func (m *Manager) Start() {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.stop != nil {
		return
	}
	stop := make(chan struct{})
	m.stop = stop
	go func() {
		m.Reconcile(context.Background(), false)
		ticker := time.NewTicker(m.interval)
		defer ticker.Stop()
		for {
			select {
			case <-stop:
				return
			case <-ticker.C:
				m.Reconcile(context.Background(), false)
			}
		}
	}()
}

// Stop stops the reconcile loop.
func (m *Manager) Stop() {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.stop != nil {
		close(m.stop)
		m.stop = nil
	}
}

// Reconcile detects the public IP once and converges every enabled entry.
func (m *Manager) Reconcile(ctx context.Context, force bool) {
	entries := m.store.List()
	if len(entries) == 0 {
		return
	}
	need4, need6 := false, false
	for _, e := range entries {
		if !e.Enabled {
			continue
		}
		if e.RecordType == "AAAA" {
			need6 = true
		} else {
			need4 = true
		}
	}
	var v4, v6 string
	var err4, err6 error
	if need4 {
		v4, err4 = DetectIP(ctx, m.client, m.ipSource)
	}
	if need6 {
		v6, err6 = DetectIP(ctx, m.client, m.ipv6Source)
	}
	for _, e := range m.store.List() {
		if !e.Enabled {
			continue
		}
		ip, ipErr := v4, err4
		if e.RecordType == "AAAA" {
			ip, ipErr = v6, err6
		}
		if ipErr != nil {
			m.record(e.ID, "error", ipErr.Error(), "")
			continue
		}
		m.runEntry(ctx, e, ip, force)
	}
}

// Run reconciles one entry, detecting the IP for its record type.
func (m *Manager) Run(ctx context.Context, id string, force bool) error {
	e, err := m.store.Get(id)
	if err != nil {
		return err
	}
	source := m.ipSource
	if e.RecordType == "AAAA" {
		source = m.ipv6Source
	}
	ip, err := DetectIP(ctx, m.client, source)
	if err != nil {
		m.record(id, "error", err.Error(), "")
		return err
	}
	return m.runEntry(ctx, e, ip, force)
}

func (m *Manager) runEntry(ctx context.Context, e Entry, ip string, force bool) error {
	if !force && e.LastIP == ip && e.LastStatus == "ok" {
		m.record(e.ID, "ok", "", "")
		return nil
	}
	if err := m.apply(ctx, e, ip); err != nil {
		m.record(e.ID, "error", err.Error(), "")
		return err
	}
	m.record(e.ID, "ok", "", ip)
	return nil
}

func (m *Manager) apply(ctx context.Context, e Entry, ip string) error {
	if m.registry == nil {
		return fmt.Errorf("no provider registry loaded")
	}
	p, ok := m.registry.Get(e.Provider)
	if !ok {
		return fmt.Errorf("unknown provider %q", e.Provider)
	}
	if !p.HasDDNS() {
		return fmt.Errorf("provider %q does not support dynamic DNS", e.Provider)
	}
	secret, err := m.readSecret(ctx, e, p)
	if err != nil {
		return err
	}
	config := make(map[string]string, len(p.DDNS.Defaults)+len(e.ProviderConfig))
	for k, v := range p.DDNS.Defaults {
		config[k] = v
	}
	for k, v := range e.ProviderConfig {
		config[k] = v
	}
	driver, err := m.newDriver(p.DDNS.Driver, m.client)
	if err != nil {
		return err
	}
	req := UpdateRequest{
		Zone:       e.Zone,
		Record:     e.Record,
		RecordType: e.RecordType,
		IP:         ip,
		TTL:        e.TTL,
		Config:     config,
		Secret:     secret,
	}
	if err := driver.Update(ctx, req); err != nil {
		// A transport error can embed the full request URL, which for the
		// generic `query` auth carries a secret. Never persist or return it.
		return sanitizeError(err, secret)
	}
	return nil
}

// sanitizeError redacts any secret value from an error before it is recorded or
// returned, so a failed request cannot leak a credential.
func sanitizeError(err error, secret map[string]string) error {
	if err == nil {
		return nil
	}
	msg := err.Error()
	for _, value := range secret {
		if len(value) >= 4 {
			msg = strings.ReplaceAll(msg, value, "***")
		}
	}
	return errors.New(msg)
}

func (m *Manager) readSecret(ctx context.Context, e Entry, p *providers.Provider) (map[string]string, error) {
	fields := p.SecretFields()
	if len(fields) == 0 {
		return nil, nil
	}
	if e.CredentialsSecret == "" {
		return nil, fmt.Errorf("provider %q needs credentials but the entry has no Secret", p.Name)
	}
	if m.clientset == nil {
		return nil, fmt.Errorf("no Kubernetes client available to read %s", e.CredentialsSecret)
	}
	client, err := m.clientset()
	if err != nil {
		return nil, fmt.Errorf("reading credentials: %w", err)
	}
	secret, err := client.CoreV1().Secrets(m.appsNamespace).Get(ctx, e.CredentialsSecret, metav1.GetOptions{})
	if err != nil {
		return nil, fmt.Errorf("reading Secret %s/%s: %w", m.appsNamespace, e.CredentialsSecret, err)
	}
	out := make(map[string]string, len(fields))
	for _, f := range fields {
		if raw, ok := secret.Data[f.SecretKeyOr()]; ok {
			out[f.Key] = string(raw)
		}
	}
	for _, f := range fields {
		if f.Required && strings.TrimSpace(out[f.Key]) == "" {
			return nil, fmt.Errorf("Secret %s is missing %s", e.CredentialsSecret, f.SecretKeyOr())
		}
	}
	return out, nil
}

func (m *Manager) record(id, status, lastErr, ip string) {
	_, err := m.store.Update(id, func(e *Entry) {
		now := m.now()
		e.LastRunAt = now
		e.NextRunAt = now.Add(m.interval)
		e.LastStatus = status
		e.LastError = lastErr
		if ip != "" {
			e.LastIP = ip
		}
	})
	if err != nil {
		// The entry was deleted between listing and recording.
		return
	}
}
