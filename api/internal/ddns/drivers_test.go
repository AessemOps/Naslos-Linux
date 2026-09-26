package ddns

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/providers"
)

func fakeGuard(*url.URL) error { return nil }

func TestOVHDriverUpdate(t *testing.T) {
	var requests []*http.Request
	var bodies []string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests = append(requests, r)
		if r.Body != nil {
			buf := make([]byte, 1024)
			n, _ := r.Body.Read(buf)
			bodies = append(bodies, string(buf[:n]))
		}
		w.Header().Set("Content-Type", "application/json")
		if r.Method == http.MethodGet {
			json.NewEncoder(w).Encode([]map[string]interface{}{{"id": 42, "target": "198.51.100.1"}})
			return
		}
		w.Write([]byte(`{}`))
	}))
	defer srv.Close()

	now := time.Unix(1700000000, 0)
	d := &OVHDriver{Client: srv.Client(), BaseURL: srv.URL, Now: func() time.Time { return now }}
	err := d.Update(context.Background(), UpdateRequest{
		Zone: "example.com", Record: "home", RecordType: "A", IP: "203.0.113.5", TTL: 300,
		Config: map[string]string{"endpoint": "ovh-eu", "applicationKey": "AK"},
		Secret: map[string]string{"applicationSecret": "AS", "consumerKey": "CK"},
	})
	if err != nil {
		t.Fatalf("update: %v", err)
	}
	if len(requests) != 3 {
		t.Fatalf("requests = %d, want 3", len(requests))
	}
	if requests[0].Method != http.MethodGet || !strings.Contains(requests[0].URL.Path, "/domain/zone/example.com/record") {
		t.Errorf("list request = %s %s", requests[0].Method, requests[0].URL)
	}
	if q := requests[0].URL.Query(); q.Get("fieldType") != "A" || q.Get("subDomain") != "home" {
		t.Errorf("list query = %s", requests[0].URL.RawQuery)
	}
	if requests[1].Method != http.MethodPut || !strings.HasSuffix(requests[1].URL.Path, "/record/42") {
		t.Errorf("update request = %s %s", requests[1].Method, requests[1].URL)
	}
	if requests[2].Method != http.MethodPost || !strings.HasSuffix(requests[2].URL.Path, "/refresh") {
		t.Errorf("refresh request = %s %s", requests[2].Method, requests[2].URL)
	}
	// The signature must match the documented scheme for the first request.
	full := srv.URL + "/domain/zone/example.com/record?fieldType=A&subDomain=home"
	want := ovhSignature("AS", "CK", http.MethodGet, full, "", now.Unix())
	if got := requests[0].Header.Get("X-Ovh-Signature"); got != want {
		t.Errorf("signature = %q, want %q", got, want)
	}
	if requests[0].Header.Get("X-Ovh-Application") != "AK" || requests[0].Header.Get("X-Ovh-Consumer") != "CK" {
		t.Errorf("OVH headers missing: %v", requests[0].Header)
	}
	if !strings.Contains(bodies[1], `"target":"203.0.113.5"`) {
		t.Errorf("update body = %s", bodies[1])
	}
}

func TestOVHDriverCreatesWhenAbsent(t *testing.T) {
	var methods []string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		methods = append(methods, r.Method)
		w.Header().Set("Content-Type", "application/json")
		if r.Method == http.MethodGet {
			w.Write([]byte(`[]`))
			return
		}
		w.Write([]byte(`{}`))
	}))
	defer srv.Close()

	d := &OVHDriver{Client: srv.Client(), BaseURL: srv.URL}
	err := d.Update(context.Background(), UpdateRequest{
		Zone: "example.com", Record: "@", RecordType: "A", IP: "203.0.113.5",
		Config: map[string]string{"applicationKey": "AK"},
		Secret: map[string]string{"applicationSecret": "AS", "consumerKey": "CK"},
	})
	if err != nil {
		t.Fatalf("update: %v", err)
	}
	if strings.Join(methods, ",") != "GET,POST,POST" {
		t.Fatalf("methods = %v, want GET,POST,POST", methods)
	}
}

func TestCloudflareDriverCreateAndUpdate(t *testing.T) {
	for _, existing := range []bool{false, true} {
		var methods []string
		var putBody string
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			methods = append(methods, r.Method)
			w.Header().Set("Content-Type", "application/json")
			switch {
			case strings.HasPrefix(r.URL.Path, "/zones") && r.URL.Path == "/zones":
				w.Write([]byte(`{"success":true,"result":[{"id":"z1"}]}`))
			case strings.Contains(r.URL.Path, "/dns_records") && r.Method == http.MethodGet:
				if existing {
					w.Write([]byte(`{"success":true,"result":[{"id":"r1"}]}`))
				} else {
					w.Write([]byte(`{"success":true,"result":[]}`))
				}
			default:
				if r.Body != nil {
					buf := make([]byte, 1024)
					n, _ := r.Body.Read(buf)
					putBody = string(buf[:n])
				}
				w.Write([]byte(`{"success":true,"result":{}}`))
			}
		}))
		d := &CloudflareDriver{Client: srv.Client(), BaseURL: srv.URL}
		err := d.Update(context.Background(), UpdateRequest{
			Zone: "example.com", Record: "home", RecordType: "A", IP: "203.0.113.5",
			Secret: map[string]string{"apiToken": "tok"},
		})
		if err != nil {
			t.Fatalf("update(existing=%v): %v", existing, err)
		}
		wantMethod := http.MethodPost
		if existing {
			wantMethod = http.MethodPut
		}
		if methods[len(methods)-1] != wantMethod {
			t.Errorf("existing=%v: last method = %s, want %s", existing, methods[len(methods)-1], wantMethod)
		}
		if !strings.Contains(putBody, `"name":"home.example.com"`) || !strings.Contains(putBody, `"content":"203.0.113.5"`) {
			t.Errorf("existing=%v: body = %s", existing, putBody)
		}
		srv.Close()
	}
}

func TestCloudflareDriverRejectsProviderErrors(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if r.URL.Path == "/zones" {
			w.Write([]byte(`{"success":true,"result":[{"id":"z1"}]}`))
			return
		}
		w.WriteHeader(http.StatusInternalServerError)
		w.Write([]byte(`{"success":false,"errors":[{"message":"boom"}]}`))
	}))
	defer srv.Close()

	d := &CloudflareDriver{Client: srv.Client(), BaseURL: srv.URL}
	err := d.Update(context.Background(), UpdateRequest{
		Zone: "example.com", Record: "home", RecordType: "A", IP: "203.0.113.5",
		Secret: map[string]string{"apiToken": "tok"},
	})
	if err == nil {
		t.Fatal("a provider 5xx must fail the update")
	}
}

func TestHTTPDriverRendersAndAuthenticates(t *testing.T) {
	var gotURL, gotBody, gotAuth, gotMethod string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotURL = r.URL.String()
		gotMethod = r.Method
		gotAuth = r.Header.Get("Authorization")
		buf := make([]byte, 1024)
		n, _ := r.Body.Read(buf)
		gotBody = string(buf[:n])
		w.Write([]byte(`{"ok":true}`))
	}))
	defer srv.Close()

	d := &HTTPDriver{Client: srv.Client(), Guard: fakeGuard, BaseURL: srv.URL}
	err := d.Update(context.Background(), UpdateRequest{
		Zone: "example.com", Record: "home", RecordType: "A", IP: "203.0.113.5", TTL: 60,
		Config: map[string]string{
			"updateUrl":       "/update?zone={{.zone}}&name={{.record}}&ip={{.ip}}",
			"method":          "PUT",
			"body":            `{"type":"{{.type}}","content":"{{.ip}}","token":"{{.secret.token}}"}`,
			"contentType":     "application/json",
			"authType":        "bearer",
			"successStatus":   "200",
			"successContains": `"ok":true`,
		},
		Secret: map[string]string{"token": "s3cr3t"},
	})
	if err != nil {
		t.Fatalf("update: %v", err)
	}
	if gotMethod != http.MethodPut {
		t.Errorf("method = %s", gotMethod)
	}
	if gotURL != "/update?zone=example.com&name=home&ip=203.0.113.5" {
		t.Errorf("url = %s", gotURL)
	}
	if gotAuth != "Bearer s3cr3t" {
		t.Errorf("auth = %q", gotAuth)
	}
	if !strings.Contains(gotBody, `"content":"203.0.113.5"`) || !strings.Contains(gotBody, `"token":"s3cr3t"`) {
		t.Errorf("body = %s", gotBody)
	}
}

func TestHTTPDriverRejectsNonPublicTargets(t *testing.T) {
	d := &HTTPDriver{Client: http.DefaultClient}
	for _, target := range []string{
		"http://127.0.0.1/update",
		"http://10.96.0.1/update",
		"http://169.254.169.254/latest/meta-data",
	} {
		raw := target
		u, _ := url.Parse(raw)
		if err := d.guard(u); err == nil {
			t.Errorf("guard accepted %s", target)
		}
	}
	// A public literal is allowed.
	u, _ := url.Parse("https://203.0.113.5/update")
	if err := d.guard(u); err != nil {
		t.Errorf("guard rejected a public IP: %v", err)
	}
}

func TestGoDaddyDriverUpdate(t *testing.T) {
	var path, auth, body string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		path = r.URL.Path
		auth = r.Header.Get("Authorization")
		buf := make([]byte, 1024)
		n, _ := r.Body.Read(buf)
		body = string(buf[:n])
		w.WriteHeader(http.StatusOK)
	}))
	defer srv.Close()

	d := &GoDaddyDriver{Client: srv.Client(), BaseURL: srv.URL + "/v1"}
	if err := d.Update(context.Background(), UpdateRequest{
		Zone: "example.com", Record: "home", RecordType: "A", IP: "203.0.113.5",
		Config: map[string]string{"apiKey": "K"},
		Secret: map[string]string{"apiSecret": "S"},
	}); err != nil {
		t.Fatalf("update: %v", err)
	}
	if path != "/v1/domains/example.com/records/A/home" {
		t.Errorf("path = %s", path)
	}
	if auth != "sso-key K:S" {
		t.Errorf("auth = %q", auth)
	}
	if !strings.Contains(body, `"data":"203.0.113.5"`) {
		t.Errorf("body = %s", body)
	}
}

func TestDigitalOceanDriverLookupAndReplace(t *testing.T) {
	var getPath, putPath, putBody string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if r.Method == http.MethodGet {
			getPath = r.URL.Path + "?" + r.URL.RawQuery
			w.Write([]byte(`{"domain_records":[{"id":7}]}`))
			return
		}
		putPath = r.URL.Path
		buf := make([]byte, 1024)
		n, _ := r.Body.Read(buf)
		putBody = string(buf[:n])
		w.Write([]byte(`{"domain_record":{"data":"203.0.113.5"}}`))
	}))
	defer srv.Close()

	d := &DigitalOceanDriver{Client: srv.Client(), BaseURL: srv.URL + "/v2"}
	err := d.Update(context.Background(), UpdateRequest{
		Zone: "example.com", Record: "home", RecordType: "A", IP: "203.0.113.5",
		Secret: map[string]string{"token": "tok"},
	})
	if err != nil {
		t.Fatalf("update: %v", err)
	}
	if !strings.Contains(getPath, "/v2/domains/example.com/records") || !strings.Contains(getPath, "name=home.example.com") {
		t.Errorf("lookup = %s", getPath)
	}
	if putPath != "/v2/domains/example.com/records/7" {
		t.Errorf("put path = %s", putPath)
	}
	if !strings.Contains(putBody, `"name":"home"`) || !strings.Contains(putBody, `"data":"203.0.113.5"`) {
		t.Errorf("put body = %s", putBody)
	}
}

func TestPorkbunDriverCreatesThenEdits(t *testing.T) {
	for _, existing := range []bool{false, true} {
		var posts []string
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			posts = append(posts, r.URL.Path)
			w.Header().Set("Content-Type", "application/json")
			if strings.Contains(r.URL.Path, "/retrieveByNameType/") {
				if existing {
					w.Write([]byte(`{"status":"SUCCESS","records":[{"id":"11"}]}`))
				} else {
					w.Write([]byte(`{"status":"SUCCESS","records":[]}`))
				}
				return
			}
			w.Write([]byte(`{"status":"SUCCESS"}`))
		}))
		d := &PorkbunDriver{Client: srv.Client(), BaseURL: srv.URL}
		err := d.Update(context.Background(), UpdateRequest{
			Zone: "example.com", Record: "home", RecordType: "A", IP: "203.0.113.5",
			Config: map[string]string{"apiKey": "K"},
			Secret: map[string]string{"secretApiKey": "S"},
		})
		if err != nil {
			t.Fatalf("update(existing=%v): %v", existing, err)
		}
		want := "/dns/create/example.com"
		if existing {
			want = "/dns/edit/example.com/11"
		}
		found := false
		for _, p := range posts {
			if p == want {
				found = true
			}
		}
		if !found {
			t.Errorf("existing=%v: posts = %v, want %s", existing, posts, want)
		}
		srv.Close()
	}
}

func TestHTTPDriverSuccessAndErrorMarkers(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("mode") == "error" {
			w.Write([]byte("badauth"))
			return
		}
		w.Write([]byte("good 203.0.113.5"))
	}))
	defer srv.Close()

	d := &HTTPDriver{Client: srv.Client(), Guard: fakeGuard, BaseURL: srv.URL}
	base := map[string]string{
		"successStatus": "200",
		"successAny":    "good,nochg",
		"errorAny":      "badauth,notfqdn",
	}
	if err := d.Update(context.Background(), UpdateRequest{Config: mergeConfig(base, map[string]string{"updateUrl": "/update"})}); err != nil {
		t.Fatalf("good response: %v", err)
	}
	if err := d.Update(context.Background(), UpdateRequest{Config: mergeConfig(base, map[string]string{"updateUrl": "/update?mode=error"})}); err == nil {
		t.Fatal("expected a badauth response to fail")
	}
}

func mergeConfig(base, extra map[string]string) map[string]string {
	out := make(map[string]string, len(base)+len(extra))
	for k, v := range base {
		out[k] = v
	}
	for k, v := range extra {
		out[k] = v
	}
	return out
}

func TestBuiltinProvidersHaveKnownDrivers(t *testing.T) {
	registry := providers.Load("")
	if errs := registry.Errors(); len(errs) > 0 {
		t.Fatalf("provider load errors: %v", errs)
	}
	ddnsCount := 0
	for _, p := range registry.List() {
		if !p.HasDDNS() {
			continue
		}
		ddnsCount++
		if _, err := defaultDriver(p.DDNS.Driver, nil); err != nil {
			t.Errorf("provider %s: %v", p.Name, err)
		}
	}
	if ddnsCount < 15 {
		t.Fatalf("expected a curated DDNS provider set, got %d", ddnsCount)
	}
}

func TestHTTPDriverStatusMismatch(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer srv.Close()
	d := &HTTPDriver{Client: srv.Client(), Guard: fakeGuard, BaseURL: srv.URL}
	err := d.Update(context.Background(), UpdateRequest{
		Config: map[string]string{"updateUrl": "/update", "successStatus": "200"},
	})
	if err == nil {
		t.Fatal("expected the status mismatch to fail")
	}
}
