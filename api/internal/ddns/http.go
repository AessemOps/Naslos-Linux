package ddns

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"text/template"
	"time"
)

// HTTPDriver renders a templated HTTP request for providers without a built-in
// protocol. The templates are operator input and are treated as admin-only.
type HTTPDriver struct {
	Client *http.Client
	// Guard validates the target URL before the request. Nil uses defaultGuard,
	// which rejects loopback, private and link-local targets (a basic SSRF
	// guard). Tests inject a permissive guard for httptest servers.
	Guard func(*url.URL) error
	// BaseURL is prepended to a relative update URL (a test seam).
	BaseURL string
}

// client returns an HTTP client that re-checks the target guard on every
// redirect and dials only an address the guard already vetted, so a redirect
// or a DNS rebind cannot reach a non-public address.
func (d *HTTPDriver) client() *http.Client {
	base := d.Client
	if base == nil {
		base = DefaultClient()
	}
	client := *base
	client.Transport = d.transport(base.Transport)
	client.CheckRedirect = func(req *http.Request, via []*http.Request) error {
		if len(via) >= 5 {
			return fmt.Errorf("too many redirects")
		}
		return d.guard(req.URL)
	}
	return &client
}

// transport wraps the base transport's dialer so the connection is made to an
// address the guard has already checked, rather than to whatever a second
// resolution returns at dial time.
func (d *HTTPDriver) transport(base http.RoundTripper) http.RoundTripper {
	transport, ok := base.(*http.Transport)
	if base == nil {
		transport, ok = http.DefaultTransport.(*http.Transport), true
	}
	if !ok || transport == nil {
		return base
	}
	cloned := transport.Clone()
	guard := d.guard
	dialer := &net.Dialer{Timeout: 10 * time.Second, KeepAlive: 30 * time.Second}
	cloned.DialContext = func(ctx context.Context, network, addr string) (net.Conn, error) {
		host, port, err := net.SplitHostPort(addr)
		if err != nil {
			return nil, err
		}
		addrs, err := net.DefaultResolver.LookupIPAddr(ctx, host)
		if err != nil {
			return nil, err
		}
		if len(addrs) == 0 {
			return nil, fmt.Errorf("%q did not resolve", host)
		}
		for _, a := range addrs {
			if err := guard(&url.URL{Scheme: "http", Host: net.JoinHostPort(a.IP.String(), "0")}); err != nil {
				return nil, err
			}
		}
		return dialer.DialContext(ctx, network, net.JoinHostPort(addrs[0].IP.String(), port))
	}
	return cloned
}

func (d *HTTPDriver) guard(u *url.URL) error {
	if d.Guard != nil {
		return d.Guard(u)
	}
	return defaultGuard(u)
}

// defaultGuard rejects targets that resolve to a non-routable address. It
// resolves hostnames too, because a name can point at the cluster or service
// CIDRs even when the literal is not an IP.
func defaultGuard(u *url.URL) error {
	host := u.Hostname()
	if host == "" {
		return fmt.Errorf("URL has no host")
	}
	if ip := net.ParseIP(host); ip != nil {
		if blockedIP(ip) {
			return fmt.Errorf("target %s is not a public address", host)
		}
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	addrs, err := net.DefaultResolver.LookupIPAddr(ctx, host)
	if err != nil {
		return fmt.Errorf("cannot resolve %q: %w", host, err)
	}
	if len(addrs) == 0 {
		return fmt.Errorf("%q did not resolve", host)
	}
	for _, addr := range addrs {
		if blockedIP(addr.IP) {
			return fmt.Errorf("target %s resolves to a non-public address %s", host, addr.IP)
		}
	}
	return nil
}

func blockedIP(ip net.IP) bool {
	return ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast() ||
		ip.IsLinkLocalMulticast() || ip.IsMulticast() || ip.IsUnspecified()
}

// Update implements Driver.
func (d *HTTPDriver) Update(ctx context.Context, r UpdateRequest) error {
	updateURL := configValue(r.Config, "updateUrl")
	if updateURL == "" {
		return fmt.Errorf("the generic HTTP driver needs updateUrl")
	}
	data := map[string]interface{}{
		"zone":   r.Zone,
		"record": r.Record,
		"type":   r.RecordType,
		"ip":     r.IP,
		"ttl":    r.TTL,
		"secret": r.Secret,
		"config": r.Config,
	}
	renderedURL, err := renderTemplate("updateUrl", updateURL, data)
	if err != nil {
		return err
	}
	if d.BaseURL != "" && strings.HasPrefix(renderedURL, "/") {
		renderedURL = strings.TrimSuffix(d.BaseURL, "/") + renderedURL
	}
	target, err := guardURL(renderedURL)
	if err != nil {
		return err
	}
	if err := d.guard(target); err != nil {
		return fmt.Errorf("refusing to call %s: %w", target, err)
	}

	method := strings.ToUpper(configValue(r.Config, "method"))
	if method == "" {
		method = http.MethodGet
	}
	switch method {
	case http.MethodGet, http.MethodPost, http.MethodPut, http.MethodPatch:
	default:
		return fmt.Errorf("unsupported method %q", method)
	}

	var body []byte
	if bodyTemplate := configValue(r.Config, "body"); bodyTemplate != "" {
		rendered, err := renderTemplate("body", bodyTemplate, data)
		if err != nil {
			return err
		}
		body = []byte(rendered)
	}

	req, err := http.NewRequestWithContext(ctx, method, target.String(), bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("User-Agent", UserAgent)
	if len(body) > 0 {
		contentType := configValue(r.Config, "contentType")
		if contentType == "" {
			contentType = "application/json"
		}
		req.Header.Set("Content-Type", contentType)
	}
	if err := applyAuth(req, r); err != nil {
		return err
	}

	resp, err := d.client().Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	// successStatus may be a single code or a comma-separated list (some
	// providers answer 204 for "no change").
	want := []int{http.StatusOK}
	if raw := configValue(r.Config, "successStatus"); raw != "" {
		want = nil
		for _, part := range strings.Split(raw, ",") {
			parsed, err := strconv.Atoi(strings.TrimSpace(part))
			if err != nil {
				return fmt.Errorf("successStatus %q is not a number", raw)
			}
			want = append(want, parsed)
		}
	}
	respBody, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	respText := string(respBody)
	statusOK := false
	for _, code := range want {
		if resp.StatusCode == code {
			statusOK = true
			break
		}
	}
	if !statusOK {
		return fmt.Errorf("update returned HTTP %d (want %v): %s", resp.StatusCode, want, truncate(respText))
	}
	// 204 No Content is an explicit "no change" from some providers; there is no
	// body to match against.
	if resp.StatusCode == http.StatusNoContent {
		return nil
	}
	// errorAny markers win over success markers (NIC providers answer 200 with
	// an error word such as "badauth").
	if markers := splitList(configValue(r.Config, "errorAny")); len(markers) > 0 {
		for _, marker := range markers {
			if strings.Contains(respText, marker) {
				return fmt.Errorf("update rejected by the provider: %s", truncate(respText))
			}
		}
	}
	if markers := splitList(configValue(r.Config, "successAny")); len(markers) > 0 {
		found := false
		for _, marker := range markers {
			if strings.Contains(respText, marker) {
				found = true
				break
			}
		}
		if !found {
			return fmt.Errorf("update response did not contain any of %v: %s", markers, truncate(respText))
		}
	}
	if contains := configValue(r.Config, "successContains"); contains != "" && !strings.Contains(respText, contains) {
		return fmt.Errorf("update response did not contain %q", contains)
	}
	return nil
}

func splitList(value string) []string {
	if strings.TrimSpace(value) == "" {
		return nil
	}
	parts := strings.Split(value, ",")
	out := make([]string, 0, len(parts))
	for _, part := range parts {
		if trimmed := strings.TrimSpace(part); trimmed != "" {
			out = append(out, trimmed)
		}
	}
	return out
}

func applyAuth(req *http.Request, r UpdateRequest) error {
	switch configValue(r.Config, "authType") {
	case "", "none":
		return nil
	case "basic":
		req.SetBasicAuth(configValue(r.Config, "username"), secretValue(r.Secret, "password"))
		return nil
	case "bearer":
		token := secretValue(r.Secret, "token")
		if token == "" {
			return fmt.Errorf("bearer auth needs a token")
		}
		req.Header.Set("Authorization", "Bearer "+token)
		return nil
	case "header":
		name := configValue(r.Config, "headerName")
		if name == "" {
			name = "Authorization"
		}
		value := secretValue(r.Secret, "headerValue")
		if value == "" {
			return fmt.Errorf("header auth needs headerValue")
		}
		req.Header.Set(name, value)
		return nil
	case "query":
		name := configValue(r.Config, "headerName")
		if name == "" {
			name = "token"
		}
		value := secretValue(r.Secret, "token")
		if value == "" {
			return fmt.Errorf("query auth needs a token")
		}
		query := req.URL.Query()
		query.Set(name, value)
		req.URL.RawQuery = query.Encode()
		return nil
	default:
		return fmt.Errorf("unknown authType %q", configValue(r.Config, "authType"))
	}
}

func renderTemplate(name, tmpl string, data map[string]interface{}) (string, error) {
	funcs := template.FuncMap{
		// fqdn builds the record's full name ("home.example.com"); label is the
		// subdomain label ("" for the apex).
		"fqdn": func(record, zone string) string { return fqdn(zone, record) },
		"label": func(record, zone string) string {
			return zoneLabel(zone, record)
		},
	}
	t, err := template.New(name).Funcs(funcs).Option("missingkey=error").Parse(tmpl)
	if err != nil {
		return "", fmt.Errorf("parsing the %s template: %w", name, err)
	}
	var buf bytes.Buffer
	if err := t.Execute(&buf, data); err != nil {
		return "", fmt.Errorf("rendering the %s template: %w", name, err)
	}
	return buf.String(), nil
}

func truncate(s string) string {
	s = strings.TrimSpace(s)
	if len(s) > 200 {
		return s[:200]
	}
	return s
}
