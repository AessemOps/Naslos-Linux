package ddns

import (
	"context"
	"encoding/json"
	"net"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/kubernetes/fake"

	"github.com/AessemOps/Naslos-Linux/api/internal/providers"
)

func TestStoreRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "ddns.json")
	store := NewStore(path)
	if err := store.Load(); err != nil {
		t.Fatalf("load: %v", err)
	}
	entry := Entry{ID: "e1", Provider: "cloudflare", Zone: "example.com", Record: "home", RecordType: "A", Enabled: true, ProviderConfig: map[string]string{"x": "y"}}
	if err := store.Put(entry); err != nil {
		t.Fatalf("put: %v", err)
	}
	reloaded := NewStore(path)
	if err := reloaded.Load(); err != nil {
		t.Fatalf("reload: %v", err)
	}
	got, err := reloaded.Get("e1")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got.Zone != "example.com" || got.ProviderConfig["x"] != "y" || !got.Enabled {
		t.Fatalf("round trip mismatch: %+v", got)
	}
}

func TestDetectIPValidatesResponse(t *testing.T) {
	cases := map[string]struct {
		body    string
		wantErr bool
	}{
		"ipv4":    {"203.0.113.7\n", false},
		"ipv6":    {"2001:db8::1\n", false},
		"garbage": {"<!doctype html>", true},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.Write([]byte(tc.body))
			}))
			defer srv.Close()
			ip, err := DetectIP(context.Background(), srv.Client(), srv.URL)
			if (err != nil) != tc.wantErr {
				t.Fatalf("DetectIP() err = %v, wantErr %v", err, tc.wantErr)
			}
			if err == nil && ip == "" {
				t.Fatal("empty IP with no error")
			}
		})
	}
	if _, err := DetectIP(context.Background(), http.DefaultClient, ""); err == nil {
		t.Fatal("expected an error for an empty source")
	}
}

type fakeDriver struct {
	calls int
	last  UpdateRequest
	err   error
}

func (f *fakeDriver) Update(_ context.Context, r UpdateRequest) error {
	f.calls++
	f.last = r
	return f.err
}

func TestManagerUpdateSkipsUnchangedAndHidesSecrets(t *testing.T) {
	ipSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("203.0.113.7"))
	}))
	defer ipSrv.Close()

	client := fake.NewSimpleClientset(&corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{Name: "naslos-ddns-e1", Namespace: "naslos-apps"},
		Data:       map[string][]byte{"api-token": []byte("super-secret-token")},
	})
	m := NewManager(Options{
		Registry:      providers.Load(""),
		StorePath:     filepath.Join(t.TempDir(), "ddns.json"),
		AppsNamespace: "naslos-apps",
		Clientset:     func() (kubernetes.Interface, error) { return client, nil },
		IPSources:     []string{ipSrv.URL},
		Interval:      time.Minute,
		Client:        ipSrv.Client(),
	})
	driver := &fakeDriver{}
	m.newDriver = func(string, *http.Client) (Driver, error) { return driver, nil }

	if err := m.Store().Put(Entry{
		ID: "e1", Provider: "cloudflare", Zone: "example.com", Record: "home",
		RecordType: "A", Enabled: true, CredentialsSecret: "naslos-ddns-e1",
		CredentialFields: []string{"apiToken"},
	}); err != nil {
		t.Fatalf("put: %v", err)
	}

	if err := m.Run(context.Background(), "e1", false); err != nil {
		t.Fatalf("run: %v", err)
	}
	if driver.calls != 1 {
		t.Fatalf("driver calls = %d, want 1", driver.calls)
	}
	if driver.last.IP != "203.0.113.7" || driver.last.Secret["apiToken"] != "super-secret-token" {
		t.Fatalf("driver request = %+v", driver.last)
	}

	// The same IP is a no-op.
	if err := m.Run(context.Background(), "e1", false); err != nil {
		t.Fatalf("second run: %v", err)
	}
	if driver.calls != 1 {
		t.Fatalf("driver calls after an unchanged IP = %d, want 1", driver.calls)
	}

	// A forced run goes through even when the IP is unchanged.
	if err := m.Run(context.Background(), "e1", true); err != nil {
		t.Fatalf("forced run: %v", err)
	}
	if driver.calls != 2 {
		t.Fatalf("driver calls after a forced run = %d, want 2", driver.calls)
	}

	got, _ := m.Store().Get("e1")
	if got.LastStatus != "ok" || got.LastIP != "203.0.113.7" || got.LastRunAt.IsZero() {
		t.Fatalf("entry status = %+v", got)
	}
	raw, _ := json.Marshal(got)
	if strings.Contains(string(raw), "super-secret-token") {
		t.Fatalf("the entry view leaked a credential value: %s", raw)
	}
}

func TestWriteSecretMergesExistingKeys(t *testing.T) {
	client := fake.NewSimpleClientset()
	ctx := context.Background()
	if err := WriteSecret(ctx, client, "naslos-apps", "creds", map[string]string{"applicationSecret": "a"}); err != nil {
		t.Fatalf("create: %v", err)
	}
	if err := WriteSecret(ctx, client, "naslos-apps", "creds", map[string]string{"consumerKey": "b"}); err != nil {
		t.Fatalf("update: %v", err)
	}
	secret, err := client.CoreV1().Secrets("naslos-apps").Get(ctx, "creds", metav1.GetOptions{})
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if string(secret.Data["applicationSecret"]) != "a" || string(secret.Data["consumerKey"]) != "b" {
		t.Fatalf("merged Secret = %v", secret.Data)
	}
}

func TestManagerRecordsCredentialErrors(t *testing.T) {
	ipSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("203.0.113.7"))
	}))
	defer ipSrv.Close()

	m := NewManager(Options{
		Registry:      providers.Load(""),
		StorePath:     filepath.Join(t.TempDir(), "ddns.json"),
		AppsNamespace: "naslos-apps",
		Clientset:     func() (kubernetes.Interface, error) { return fake.NewSimpleClientset(), nil },
		IPSources:     []string{ipSrv.URL},
		Interval:      time.Minute,
		Client:        ipSrv.Client(),
	})
	m.newDriver = func(string, *http.Client) (Driver, error) { return &fakeDriver{}, nil }
	_ = m.Store().Put(Entry{ID: "e2", Provider: "cloudflare", Zone: "example.com", Record: "home", RecordType: "A", Enabled: true, CredentialsSecret: "missing"})

	if err := m.Run(context.Background(), "e2", false); err == nil {
		t.Fatal("expected a missing Secret to fail the run")
	}
	got, _ := m.Store().Get("e2")
	if got.LastStatus != "error" || got.LastError == "" {
		t.Fatalf("entry status = %+v", got)
	}
}

type fakeResolver struct {
	ips []string
	err error
}

func (f fakeResolver) LookupIPAddr(_ context.Context, _ string) ([]net.IPAddr, error) {
	if f.err != nil {
		return nil, f.err
	}
	out := make([]net.IPAddr, 0, len(f.ips))
	for _, value := range f.ips {
		out = append(out, net.IPAddr{IP: net.ParseIP(value)})
	}
	return out, nil
}

func newManagerWithResolver(t *testing.T, resolver Resolver) (*Manager, *fakeDriver, *httptest.Server) {
	t.Helper()
	ipSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("203.0.113.7"))
	}))
	t.Cleanup(ipSrv.Close)
	client := fake.NewSimpleClientset(&corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{Name: "naslos-ddns-e1", Namespace: "naslos-apps"},
		Data:       map[string][]byte{"api-token": []byte("tok")},
	})
	m := NewManager(Options{
		Registry:      providers.Load(""),
		StorePath:     filepath.Join(t.TempDir(), "ddns.json"),
		AppsNamespace: "naslos-apps",
		Clientset:     func() (kubernetes.Interface, error) { return client, nil },
		IPSources:     []string{ipSrv.URL},
		Interval:      time.Minute,
		Cooldown:      time.Minute,
		Client:        ipSrv.Client(),
		Resolver:      resolver,
	})
	driver := &fakeDriver{}
	m.newDriver = func(string, *http.Client) (Driver, error) { return driver, nil }
	return m, driver, ipSrv
}

// TestManagerSkipsWhenDNSAlreadyHoldsTheIP is the ddns-updater detection model:
// when the record already resolves to the public IP, no provider call is made.
func TestManagerSkipsWhenDNSAlreadyHoldsTheIP(t *testing.T) {
	m, driver, _ := newManagerWithResolver(t, fakeResolver{ips: []string{"203.0.113.7"}})
	_ = m.Store().Put(Entry{ID: "e1", Provider: "cloudflare", Zone: "example.com", Record: "home", RecordType: "A", Enabled: true, CredentialsSecret: "naslos-ddns-e1"})

	if err := m.Run(context.Background(), "e1", false); err != nil {
		t.Fatalf("run: %v", err)
	}
	if driver.calls != 0 {
		t.Fatalf("driver calls = %d, want 0 (DNS already correct)", driver.calls)
	}
	got, _ := m.Store().Get("e1")
	if got.LastIP != "203.0.113.7" || got.LastStatus != "ok" {
		t.Fatalf("entry = %+v", got)
	}
}

// TestManagerUpdatesWhenDNSDiffersAndRespectsCooldown covers the update path and
// the per-record cooldown.
func TestManagerUpdatesWhenDNSDiffersAndRespectsCooldown(t *testing.T) {
	m, driver, _ := newManagerWithResolver(t, fakeResolver{ips: []string{"198.51.100.1"}})
	_ = m.Store().Put(Entry{ID: "e1", Provider: "cloudflare", Zone: "example.com", Record: "home", RecordType: "A", Enabled: true, CredentialsSecret: "naslos-ddns-e1"})

	if err := m.Run(context.Background(), "e1", false); err != nil {
		t.Fatalf("run: %v", err)
	}
	if driver.calls != 1 {
		t.Fatalf("driver calls = %d, want 1", driver.calls)
	}
	got, _ := m.Store().Get("e1")
	if got.LastUpdateAt.IsZero() {
		t.Fatal("LastUpdateAt was not recorded")
	}
	// Inside the cooldown, a non-forced run must not call the provider again.
	if err := m.Run(context.Background(), "e1", false); err != nil {
		t.Fatalf("second run: %v", err)
	}
	if driver.calls != 1 {
		t.Fatalf("driver calls inside cooldown = %d, want 1", driver.calls)
	}
	// A forced run bypasses both the cooldown and the DNS pre-check.
	if err := m.Run(context.Background(), "e1", true); err != nil {
		t.Fatalf("forced run: %v", err)
	}
	if driver.calls != 2 {
		t.Fatalf("driver calls after forced run = %d, want 2", driver.calls)
	}
}

// TestManagerProxiedUsesStoredIP verifies the Cloudflare-proxied fallback.
func TestManagerProxiedUsesStoredIP(t *testing.T) {
	m, driver, _ := newManagerWithResolver(t, fakeResolver{ips: []string{"198.51.100.1"}})
	_ = m.Store().Put(Entry{
		ID: "e1", Provider: "cloudflare", Zone: "example.com", Record: "home", RecordType: "A",
		Enabled: true, CredentialsSecret: "naslos-ddns-e1",
		ProviderConfig: map[string]string{"proxied": "true"},
		LastIP:         "203.0.113.7", LastStatus: "ok",
	})

	if err := m.Run(context.Background(), "e1", false); err != nil {
		t.Fatalf("run: %v", err)
	}
	if driver.calls != 0 {
		t.Fatalf("driver calls = %d, want 0 (proxied record already converged)", driver.calls)
	}
}

// TestManagerUpdatesWhenDNSLookupFails ensures a failed lookup updates rather
// than silently skipping.
func TestManagerUpdatesWhenDNSLookupFails(t *testing.T) {
	m, driver, _ := newManagerWithResolver(t, fakeResolver{err: &net.DNSError{IsNotFound: true, Err: "no such host"}})
	_ = m.Store().Put(Entry{ID: "e1", Provider: "cloudflare", Zone: "example.com", Record: "home", RecordType: "A", Enabled: true, CredentialsSecret: "naslos-ddns-e1"})

	if err := m.Run(context.Background(), "e1", false); err != nil {
		t.Fatalf("run: %v", err)
	}
	if driver.calls != 1 {
		t.Fatalf("driver calls = %d, want 1 (lookup failure must update)", driver.calls)
	}
}

func TestDetectIPAnyCyclesAndChecksFamily(t *testing.T) {
	bad := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer bad.Close()
	good := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("203.0.113.9"))
	}))
	defer good.Close()

	ip, err := DetectIPAny(context.Background(), good.Client(), false, []string{bad.URL, good.URL})
	if err != nil || ip != "203.0.113.9" {
		t.Fatalf("DetectIPAny = %q, %v", ip, err)
	}
	// A v4 address must be rejected when a v6 address is requested.
	if _, err := DetectIPAny(context.Background(), good.Client(), true, []string{good.URL}); err == nil {
		t.Fatal("expected the wrong address family to fail")
	}
	// An unknown DNS source is rejected.
	if _, err := DetectIPAny(context.Background(), good.Client(), false, []string{"dns:nope"}); err == nil {
		t.Fatal("expected an unknown DNS source to fail")
	}
}
