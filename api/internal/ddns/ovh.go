package ddns

import (
	"bytes"
	"context"
	"crypto/sha1"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

// OVHDriver updates a record through OVH. Two modes, matching ddns-updater:
// "dynamic" (default) uses the DynHost service with username/password, "api"
// uses the signed ZoneDNS API with an application key/secret and consumer key.
type OVHDriver struct {
	Client *http.Client
	// BaseURL overrides the endpoint-derived API base (tests point it at an
	// httptest server).
	BaseURL string
	// DynHostBase overrides https://www.ovh.com for DynHost (tests).
	DynHostBase string
	// Now is a test seam for the request timestamp.
	Now func() time.Time
}

const ovhDynHostBase = "https://www.ovh.com"

func (d *OVHDriver) client() *http.Client {
	if d.Client != nil {
		return d.Client
	}
	return DefaultClient()
}

func (d *OVHDriver) now() time.Time {
	if d.Now != nil {
		return d.Now()
	}
	return time.Now()
}

// ovhBase maps an OVH endpoint name to its API base URL.
func ovhBase(endpoint string) (string, error) {
	switch endpoint {
	case "", "ovh-eu":
		return "https://eu.api.ovh.com/1.0", nil
	case "ovh-ca":
		return "https://ca.api.ovh.com/1.0", nil
	case "ovh-us":
		return "https://api.us.ovhcloud.com/1.0", nil
	}
	if bytes.ContainsRune([]byte(endpoint), '.') {
		return "https://" + endpoint + "/1.0", nil
	}
	return "", fmt.Errorf("unknown OVH endpoint %q", endpoint)
}

// ovhSignature is the OVH API signing scheme ($1$ + SHA1 hex).
func ovhSignature(applicationSecret, consumerKey, method, fullURL, body string, ts int64) string {
	raw := applicationSecret + "+" + consumerKey + "+" + method + "+" + fullURL + "+" + body + "+" + strconv.FormatInt(ts, 10)
	sum := sha1.Sum([]byte(raw))
	return "$1$" + hex.EncodeToString(sum[:])
}

type ovhRecord struct {
	ID     int64  `json:"id"`
	Target string `json:"target"`
}

// Update implements Driver.
func (d *OVHDriver) Update(ctx context.Context, r UpdateRequest) error {
	if !strings.EqualFold(configValue(r.Config, "mode"), "api") {
		return d.updateDynHost(ctx, r)
	}
	return d.updateZoneDNS(ctx, r)
}

// updateDynHost uses OVH's DynHost service: GET /nic/update with the DynHost
// username/password as HTTP Basic auth.
func (d *OVHDriver) updateDynHost(ctx context.Context, r UpdateRequest) error {
	username := configValue(r.Config, "username")
	password := secretValue(r.Secret, "password")
	if username == "" || password == "" {
		return fmt.Errorf("OVH DynHost mode requires a username and password (or set mode: api)")
	}
	base := d.DynHostBase
	if base == "" {
		base = ovhDynHostBase
	}
	query := url.Values{}
	query.Set("system", "dyndns")
	query.Set("hostname", fqdn(r.Zone, r.Record))
	query.Set("myip", r.IP)
	target := strings.TrimSuffix(base, "/") + "/nic/update?" + query.Encode()

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return err
	}
	req.SetBasicAuth(username, password)
	req.Header.Set("User-Agent", UserAgent)
	resp, err := d.client().Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	body, _ := ioReadLimited(resp)
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("OVH DynHost returned HTTP %d: %s", resp.StatusCode, truncate(string(body)))
	}
	text := strings.ToLower(strings.TrimSpace(string(body)))
	switch {
	case strings.HasPrefix(text, "good"), strings.HasPrefix(text, "nochg"):
		return nil
	case strings.HasPrefix(text, "nohost"), strings.HasPrefix(text, "notfqdn"):
		return fmt.Errorf("OVH DynHost: hostname does not exist")
	case strings.HasPrefix(text, "badrequest"):
		return fmt.Errorf("OVH DynHost: bad request")
	case strings.HasPrefix(text, "badauth"):
		return fmt.Errorf("OVH DynHost: authentication failed")
	default:
		return fmt.Errorf("OVH DynHost: unexpected response %q", truncate(string(body)))
	}
}

// updateZoneDNS uses the signed OVH API (mode: api).
func (d *OVHDriver) updateZoneDNS(ctx context.Context, r UpdateRequest) error {
	endpoint := configValue(r.Config, "endpoint")
	base := d.BaseURL
	if base == "" {
		var err error
		base, err = ovhBase(endpoint)
		if err != nil {
			return err
		}
	}
	appKey := configValue(r.Config, "applicationKey")
	appSecret := secretValue(r.Secret, "applicationSecret")
	consumerKey := secretValue(r.Secret, "consumerKey")
	if appKey == "" || appSecret == "" || consumerKey == "" {
		return fmt.Errorf("OVH requires applicationKey, applicationSecret and consumerKey")
	}
	fieldType := r.RecordType
	label := zoneLabel(r.Zone, r.Record)
	zone := url.PathEscape(r.Zone)

	listURL := base + "/domain/zone/" + zone + "/record?fieldType=" +
		url.QueryEscape(fieldType) + "&subDomain=" + url.QueryEscape(label)
	resp, err := d.do(ctx, http.MethodGet, listURL, appKey, appSecret, consumerKey, nil)
	if err != nil {
		return err
	}
	if err := checkStatus(resp, http.StatusOK); err != nil {
		resp.Body.Close()
		return err
	}
	var records []ovhRecord
	decodeErr := json.NewDecoder(resp.Body).Decode(&records)
	resp.Body.Close()
	if decodeErr != nil {
		return fmt.Errorf("parsing the OVH record list: %w", decodeErr)
	}

	payload, err := json.Marshal(map[string]interface{}{
		"target":    r.IP,
		"ttl":       normalizeTTL(r.TTL),
		"fieldType": fieldType,
		"subDomain": label,
	})
	if err != nil {
		return err
	}
	if len(records) > 0 {
		updateURL := base + "/domain/zone/" + zone + "/record/" + strconv.FormatInt(records[0].ID, 10)
		body, _ := json.Marshal(map[string]interface{}{"target": r.IP, "ttl": normalizeTTL(r.TTL)})
		resp, err := d.do(ctx, http.MethodPut, updateURL, appKey, appSecret, consumerKey, body)
		if err != nil {
			return err
		}
		defer resp.Body.Close()
		if err := checkStatus(resp, http.StatusOK); err != nil {
			return err
		}
	} else {
		createURL := base + "/domain/zone/" + zone + "/record"
		resp, err := d.do(ctx, http.MethodPost, createURL, appKey, appSecret, consumerKey, payload)
		if err != nil {
			return err
		}
		defer resp.Body.Close()
		if err := checkStatus(resp, http.StatusOK); err != nil {
			return err
		}
	}

	refreshURL := base + "/domain/zone/" + zone + "/refresh"
	resp, err = d.do(ctx, http.MethodPost, refreshURL, appKey, appSecret, consumerKey, []byte{})
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	return checkStatus(resp, http.StatusOK)
}

func (d *OVHDriver) do(ctx context.Context, method, fullURL, appKey, appSecret, consumerKey string, body []byte) (*http.Response, error) {
	ts := d.now().Unix()
	sig := ovhSignature(appSecret, consumerKey, method, fullURL, string(body), ts)
	var reader *bytes.Reader
	if body == nil {
		reader = bytes.NewReader(nil)
	} else {
		reader = bytes.NewReader(body)
	}
	req, err := http.NewRequestWithContext(ctx, method, fullURL, reader)
	if err != nil {
		return nil, err
	}
	req.Header.Set("X-Ovh-Application", appKey)
	req.Header.Set("X-Ovh-Consumer", consumerKey)
	req.Header.Set("X-Ovh-Timestamp", strconv.FormatInt(ts, 10))
	req.Header.Set("X-Ovh-Signature", sig)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("User-Agent", UserAgent)
	return d.client().Do(req)
}
