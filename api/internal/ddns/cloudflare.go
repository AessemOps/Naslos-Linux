package ddns

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strings"
)

// CloudflareDriver updates a record through the Cloudflare API v4.
type CloudflareDriver struct {
	Client *http.Client
	// BaseURL overrides the API base (tests point it at an httptest server).
	BaseURL string
}

const cloudflareDefaultBase = "https://api.cloudflare.com/client/v4"

func (d *CloudflareDriver) client() *http.Client {
	if d.Client != nil {
		return d.Client
	}
	return DefaultClient()
}

func (d *CloudflareDriver) base() string {
	if d.BaseURL != "" {
		return d.BaseURL
	}
	return cloudflareDefaultBase
}

type cloudflareEnvelope struct {
	Success bool            `json:"success"`
	Errors  json.RawMessage `json:"errors"`
	Result  json.RawMessage `json:"result"`
}

type cloudflareZone struct {
	ID string `json:"id"`
}

type cloudflareRecord struct {
	ID string `json:"id"`
}

func (d *CloudflareDriver) do(ctx context.Context, method, fullURL, token string, body []byte) (*http.Response, error) {
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
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("User-Agent", UserAgent)
	return d.client().Do(req)
}

// decodeEnvelope reads a Cloudflare response and returns its result.
func decodeEnvelope(resp *http.Response, into interface{}) error {
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		body, _ := ioReadLimited(resp)
		return fmt.Errorf("Cloudflare returned HTTP %d: %s", resp.StatusCode, strings.TrimSpace(string(body)))
	}
	var env cloudflareEnvelope
	if err := json.NewDecoder(resp.Body).Decode(&env); err != nil {
		return fmt.Errorf("parsing the Cloudflare response: %w", err)
	}
	if !env.Success {
		return fmt.Errorf("Cloudflare API error: %s", string(env.Errors))
	}
	if into != nil && len(env.Result) > 0 {
		if err := json.Unmarshal(env.Result, into); err != nil {
			return fmt.Errorf("parsing the Cloudflare result: %w", err)
		}
	}
	return nil
}

// Update implements Driver.
func (d *CloudflareDriver) Update(ctx context.Context, r UpdateRequest) error {
	token := secretValue(r.Secret, "apiToken")
	if token == "" {
		return fmt.Errorf("Cloudflare requires an apiToken")
	}
	name := fqdn(r.Zone, r.Record)

	zoneResp, err := d.do(ctx, http.MethodGet, d.base()+"/zones?name="+url.QueryEscape(r.Zone), token, nil)
	if err != nil {
		return err
	}
	var zones []cloudflareZone
	if err := decodeEnvelope(zoneResp, &zones); err != nil {
		return err
	}
	if len(zones) == 0 {
		return fmt.Errorf("Cloudflare has no zone %q", r.Zone)
	}
	zoneID := zones[0].ID

	listURL := d.base() + "/zones/" + url.PathEscape(zoneID) + "/dns_records?type=" +
		url.QueryEscape(r.RecordType) + "&name=" + url.QueryEscape(name)
	listResp, err := d.do(ctx, http.MethodGet, listURL, token, nil)
	if err != nil {
		return err
	}
	var records []cloudflareRecord
	if err := decodeEnvelope(listResp, &records); err != nil {
		return err
	}

	ttl := normalizeTTL(r.TTL)
	if ttl == 0 {
		// Cloudflare treats 1 as "automatic".
		ttl = 1
	}
	payload, err := json.Marshal(map[string]interface{}{
		"type":    r.RecordType,
		"name":    name,
		"content": r.IP,
		"ttl":     ttl,
		"proxied": false,
	})
	if err != nil {
		return err
	}

	method := http.MethodPost
	target := d.base() + "/zones/" + url.PathEscape(zoneID) + "/dns_records"
	if len(records) > 0 {
		method = http.MethodPut
		target += "/" + url.PathEscape(records[0].ID)
	}
	resp, err := d.do(ctx, method, target, token, payload)
	if err != nil {
		return err
	}
	return decodeEnvelope(resp, nil)
}
