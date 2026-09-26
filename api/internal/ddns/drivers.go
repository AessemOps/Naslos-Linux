package ddns

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

// UserAgent identifies the API to provider APIs.
const UserAgent = "naslos-api/ddns"

// UpdateRequest is one DDNS update for a driver.
type UpdateRequest struct {
	Zone       string
	Record     string
	RecordType string
	IP         string
	TTL        int
	// Config is the entry's non-secret provider config merged with the driver
	// defaults.
	Config map[string]string
	// Secret holds the resolved secret fields (never persisted).
	Secret map[string]string
}

// Driver applies one DDNS update.
type Driver interface {
	Update(ctx context.Context, r UpdateRequest) error
}

// DefaultClient is the shared HTTP client for provider APIs.
func DefaultClient() *http.Client {
	return &http.Client{Timeout: 15 * time.Second}
}

func defaultDriver(driver string, client *http.Client) (Driver, error) {
	if client == nil {
		client = DefaultClient()
	}
	switch driver {
	case "ovh":
		return &OVHDriver{Client: client}, nil
	case "cloudflare":
		return &CloudflareDriver{Client: client}, nil
	case "http":
		return &HTTPDriver{Client: client}, nil
	default:
		return nil, fmt.Errorf("unknown DDNS driver %q", driver)
	}
}

func corev1Secret(name string, data map[string][]byte) *corev1.Secret {
	return &corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{Name: name},
		Data:       data,
	}
}

// fqdn builds the record's fully qualified name.
func fqdn(zone, record string) string {
	record = strings.TrimSpace(record)
	zone = strings.TrimSpace(zone)
	if record == "" || record == "@" {
		return zone
	}
	if record == zone || strings.HasSuffix(record, "."+zone) {
		return record
	}
	return record + "." + zone
}

// zoneLabel is the record's subdomain label ("" for the apex).
func zoneLabel(zone, record string) string {
	record = strings.TrimSpace(record)
	if record == "" || record == "@" || record == zone {
		return ""
	}
	return strings.TrimSuffix(record, "."+zone)
}

func configValue(config map[string]string, key string) string {
	if config == nil {
		return ""
	}
	return strings.TrimSpace(config[key])
}

func secretValue(secret map[string]string, key string) string {
	if secret == nil {
		return ""
	}
	return strings.TrimSpace(secret[key])
}

// normalizeTTL keeps well-known TTL sentinels: 0 means "unspecified" and is
// reported to the provider as its own default.
func normalizeTTL(ttl int) int {
	if ttl <= 0 {
		return 0
	}
	return ttl
}

// checkStatus errors on an unexpected HTTP status.
func checkStatus(resp *http.Response, want int) error {
	if resp.StatusCode == want {
		return nil
	}
	body, _ := ioReadLimited(resp)
	return fmt.Errorf("provider returned HTTP %d: %s", resp.StatusCode, strings.TrimSpace(string(body)))
}

func ioReadLimited(resp *http.Response) ([]byte, error) {
	return io.ReadAll(io.LimitReader(resp.Body, 4096))
}

func guardURL(raw string) (*url.URL, error) {
	u, err := url.Parse(raw)
	if err != nil {
		return nil, fmt.Errorf("invalid URL %q: %w", raw, err)
	}
	if u.Scheme != "http" && u.Scheme != "https" {
		return nil, fmt.Errorf("unsupported URL scheme %q", u.Scheme)
	}
	if u.Host == "" {
		return nil, fmt.Errorf("URL %q has no host", raw)
	}
	return u, nil
}
