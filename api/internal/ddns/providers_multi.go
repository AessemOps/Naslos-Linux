// The DigitalOcean, GoDaddy and Porkbun drivers are ported from
// qdm12/ddns-updater (MIT); see CREDITS.md.
package ddns

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
)

// GoDaddyDriver updates a record through the GoDaddy Domains API v1.
type GoDaddyDriver struct {
	Client  *http.Client
	BaseURL string
}

const godaddyDefaultBase = "https://api.godaddy.com/v1"

func (d *GoDaddyDriver) base() string {
	if d.BaseURL != "" {
		return d.BaseURL
	}
	return godaddyDefaultBase
}

// Update implements Driver.
func (d *GoDaddyDriver) Update(ctx context.Context, r UpdateRequest) error {
	key := configValue(r.Config, "apiKey")
	secret := secretValue(r.Secret, "apiSecret")
	if key == "" || secret == "" {
		return fmt.Errorf("GoDaddy requires apiKey and apiSecret")
	}
	owner := zoneLabel(r.Zone, r.Record)
	if owner == "" {
		owner = "@"
	}
	body, err := json.Marshal([]map[string]string{{"data": r.IP}})
	if err != nil {
		return err
	}
	target := d.base() + "/domains/" + r.Zone + "/records/" + r.RecordType + "/" + owner
	req, err := http.NewRequestWithContext(ctx, http.MethodPut, target, bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "sso-key "+key+":"+secret)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", UserAgent)
	client := d.Client
	if client == nil {
		client = DefaultClient()
	}
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		raw, _ := ioReadLimited(resp)
		return fmt.Errorf("GoDaddy returned HTTP %d: %s", resp.StatusCode, truncate(string(raw)))
	}
	return nil
}

// DigitalOceanDriver looks the record up by name, then replaces it by id.
type DigitalOceanDriver struct {
	Client  *http.Client
	BaseURL string
}

const digitalOceanDefaultBase = "https://api.digitalocean.com/v2"

func (d *DigitalOceanDriver) base() string {
	if d.BaseURL != "" {
		return d.BaseURL
	}
	return digitalOceanDefaultBase
}

func (d *DigitalOceanDriver) client() *http.Client {
	if d.Client != nil {
		return d.Client
	}
	return DefaultClient()
}

func (d *DigitalOceanDriver) do(ctx context.Context, method, target, token string, body []byte) (*http.Response, error) {
	var reader *bytes.Reader
	if body == nil {
		reader = bytes.NewReader(nil)
	} else {
		reader = bytes.NewReader(body)
	}
	req, err := http.NewRequestWithContext(ctx, method, target, reader)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", UserAgent)
	return d.client().Do(req)
}

// Update implements Driver.
func (d *DigitalOceanDriver) Update(ctx context.Context, r UpdateRequest) error {
	token := secretValue(r.Secret, "token")
	if token == "" {
		return fmt.Errorf("DigitalOcean requires a token")
	}
	name := fqdn(r.Zone, r.Record)
	listURL := d.base() + "/domains/" + r.Zone + "/records?name=" + name + "&type=" + r.RecordType
	resp, err := d.do(ctx, http.MethodGet, listURL, token, nil)
	if err != nil {
		return err
	}
	var listing struct {
		Records []struct {
			ID int64 `json:"id"`
		} `json:"domain_records"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&listing); err != nil {
		resp.Body.Close()
		return fmt.Errorf("parsing the DigitalOcean record list: %w", err)
	}
	resp.Body.Close()
	if len(listing.Records) == 0 {
		return fmt.Errorf("DigitalOcean has no %s record for %s", r.RecordType, name)
	}
	owner := zoneLabel(r.Zone, r.Record)
	if owner == "" {
		owner = "@"
	}
	body, err := json.Marshal(map[string]string{"type": r.RecordType, "name": owner, "data": r.IP})
	if err != nil {
		return err
	}
	target := fmt.Sprintf("%s/domains/%s/records/%d", d.base(), r.Zone, listing.Records[0].ID)
	resp, err = d.do(ctx, http.MethodPut, target, token, body)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		raw, _ := ioReadLimited(resp)
		return fmt.Errorf("DigitalOcean returned HTTP %d: %s", resp.StatusCode, truncate(string(raw)))
	}
	return nil
}

// PorkbunDriver looks the record up, then creates or edits it.
type PorkbunDriver struct {
	Client  *http.Client
	BaseURL string
}

const porkbunDefaultBase = "https://api.porkbun.com/api/json/v3"

func (d *PorkbunDriver) base() string {
	if d.BaseURL != "" {
		return d.BaseURL
	}
	return porkbunDefaultBase
}

func (d *PorkbunDriver) post(ctx context.Context, target string, payload map[string]interface{}) (map[string]interface{}, error) {
	body, err := json.Marshal(payload)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, target, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", UserAgent)
	client := d.Client
	if client == nil {
		client = DefaultClient()
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	raw, _ := ioReadLimited(resp)
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("Porkbun returned HTTP %d: %s", resp.StatusCode, truncate(string(raw)))
	}
	var decoded map[string]interface{}
	if err := json.Unmarshal(raw, &decoded); err != nil {
		return nil, fmt.Errorf("parsing the Porkbun response: %w", err)
	}
	if status, _ := decoded["status"].(string); !strings.EqualFold(status, "SUCCESS") {
		return nil, fmt.Errorf("Porkbun error: %s", truncate(string(raw)))
	}
	return decoded, nil
}

// Update implements Driver.
func (d *PorkbunDriver) Update(ctx context.Context, r UpdateRequest) error {
	key := configValue(r.Config, "apiKey")
	secret := secretValue(r.Secret, "secretApiKey")
	if key == "" || secret == "" {
		return fmt.Errorf("Porkbun requires apiKey and secretApiKey")
	}
	owner := zoneLabel(r.Zone, r.Record)
	auth := func(extra map[string]interface{}) map[string]interface{} {
		out := map[string]interface{}{"apikey": key, "secretapikey": secret}
		for k, v := range extra {
			out[k] = v
		}
		return out
	}
	ttl := r.TTL
	if ttl <= 0 {
		ttl = 0
	}

	listTarget := d.base() + "/dns/retrieveByNameType/" + r.Zone + "/" + r.RecordType + "/" + owner
	decoded, err := d.post(ctx, listTarget, auth(nil))
	if err != nil {
		return err
	}
	ids := porkbunRecordIDs(decoded["records"])

	fields := map[string]interface{}{
		"content": r.IP,
		"type":    r.RecordType,
		"ttl":     fmt.Sprintf("%d", ttl),
	}
	if owner != "" {
		fields["name"] = owner
	}
	if len(ids) == 0 {
		if _, err := d.post(ctx, d.base()+"/dns/create/"+r.Zone, auth(fields)); err != nil {
			return fmt.Errorf("creating the Porkbun record: %w", err)
		}
		return nil
	}
	for _, id := range ids {
		target := fmt.Sprintf("%s/dns/edit/%s/%s", d.base(), r.Zone, id)
		if _, err := d.post(ctx, target, auth(fields)); err != nil {
			return fmt.Errorf("editing Porkbun record %s: %w", id, err)
		}
	}
	return nil
}

func porkbunRecordIDs(raw interface{}) []string {
	list, ok := raw.([]interface{})
	if !ok {
		return nil
	}
	out := make([]string, 0, len(list))
	for _, item := range list {
		record, ok := item.(map[string]interface{})
		if !ok {
			continue
		}
		if id, ok := record["id"].(string); ok && id != "" {
			out = append(out, id)
		}
	}
	return out
}
