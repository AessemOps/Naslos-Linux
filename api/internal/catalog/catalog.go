// Package catalog builds the Naslos app catalog from git-based chart
// repositories. Each app is a chart directory with a sibling `naslos-app.yaml`
// install-config; user-added repositories override the official repository on
// name collision.
package catalog

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"

	"gopkg.in/yaml.v3"

	"github.com/AessemOps/Naslos-Linux/api/internal/chartsrepo"
)

// dns1123Label matches a lowercase RFC 1123 label (DNS-1123 label).
var dns1123Label = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`)

// releaseNameTemplate is the only template allowed in service names.
const releaseNameTemplate = "{{ .Release.Name }}"

// manifestFile is the install-config beside each chart.
const manifestFile = "naslos-app.yaml"

// chartFile carries the chart name/version that wins for Helm.
const chartFile = "Chart.yaml"

// Catalog is an immutable snapshot of apps from a set of repositories.
type Catalog struct {
	apps map[string]*App
}

// App is a catalog entry for an installable app.
type App struct {
	Name          string                 `json:"name"`
	DisplayName   string                 `json:"displayName"`
	Description   string                 `json:"description"`
	Category      string                 `json:"category"`
	Icon          string                 `json:"icon"`
	Version       string                 `json:"version"`
	Chart         string                 `json:"chart"`
	Repository    string                 `json:"repository"`
	Schema        json.RawMessage        `json:"schema"`
	DefaultValues map[string]interface{} `json:"defaultValues"`
	Ports         []int                  `json:"ports"`
	Website       string                 `json:"website"`
	Tags          []string               `json:"tags"`

	// Source is the repository the app came from.
	Source string `json:"source"`
	// Channel is the channel whose manifest is served by default.
	Channel string `json:"channel"`
	// Channels are the channels this app is available in.
	Channels []string `json:"channels"`
	// ChartPath is the chart directory relative to the repository root.
	ChartPath string `json:"chartPath"`

	// Services declare what the exposure layer routes to.
	Services []Service `json:"services"`
	// Exposure holds the install UI's exposure defaults.
	Exposure ExposureDefaults `json:"exposure"`
}

// Service is a route target declared by an app manifest.
type Service struct {
	// Name may be a literal or the `{{ .Release.Name }}` template.
	Name   string `json:"name"`
	Port   int    `json:"port"`
	Scheme string `json:"scheme"`
}

// ExposureDefaults are the exposure toggles shown by the install UI.
type ExposureDefaults struct {
	Subdomain string `json:"subdomain"`
	TLS       bool   `json:"tls"`
	Auth      bool   `json:"auth"`
	LocalOnly bool   `json:"localOnly"`
}

// CatalogEntry is the catalog index response.
type CatalogEntry struct {
	Name        string   `json:"name"`
	DisplayName string   `json:"displayName"`
	Description string   `json:"description"`
	Category    string   `json:"category"`
	Icon        string   `json:"icon"`
	Version     string   `json:"version"`
	Tags        []string `json:"tags"`
	Source      string   `json:"source"`
	Channel     string   `json:"channel"`
	Channels    []string `json:"channels"`
	ChartPath   string   `json:"chartPath"`
}

// SourceRef is one repository/channel working tree to scan.
type SourceRef struct {
	Name        string
	DisplayName string
	Channel     string
	Dir         string
	Official    bool
}

// manifest is the parsed naslos-app.yaml.
type manifest struct {
	Name          string                 `yaml:"name"`
	DisplayName   string                 `yaml:"displayName"`
	Description   string                 `yaml:"description"`
	Category      string                 `yaml:"category"`
	Icon          string                 `yaml:"icon"`
	Website       string                 `yaml:"website"`
	Version       string                 `yaml:"version"`
	Tags          []string               `yaml:"tags"`
	Schema        map[string]interface{} `yaml:"schema"`
	DefaultValues map[string]interface{} `yaml:"defaultValues"`
	Services      []Service              `yaml:"services"`
	Exposure      ExposureDefaults       `yaml:"exposure"`
}

// candidate is one app found in one source/channel.
type candidate struct {
	app   *App
	ref   SourceRef
	ports []int
}

// Load scans every source/channel working tree and merges them into a catalog.
// A source that has not been cloned (missing directory) is skipped, not fatal.
func Load(refs []SourceRef) *Catalog {
	c := &Catalog{apps: make(map[string]*App)}
	byName := make(map[string][]candidate)

	for _, ref := range refs {
		for _, cand := range scanRef(ref) {
			byName[cand.app.Name] = append(byName[cand.app.Name], cand)
		}
	}

	for name, candidates := range byName {
		winner := pickWinner(candidates)
		app := winner.app

		channels := make([]string, 0, len(candidates))
		seen := make(map[string]struct{})
		for _, cand := range candidates {
			if cand.ref.Name != winner.ref.Name {
				continue
			}
			if _, ok := seen[cand.ref.Channel]; ok {
				continue
			}
			seen[cand.ref.Channel] = struct{}{}
			channels = append(channels, cand.ref.Channel)
		}
		sort.Strings(channels)
		app.Channels = channels

		c.apps[name] = app
	}

	return c
}

// pickWinner applies user-over-official precedence, then prefers the Prod
// channel and finally a stable alphabetical order.
func pickWinner(candidates []candidate) candidate {
	official := make([]candidate, 0, len(candidates))
	user := make([]candidate, 0, len(candidates))
	for _, cand := range candidates {
		if cand.ref.Official {
			official = append(official, cand)
		} else {
			user = append(user, cand)
		}
	}
	pool := user
	if len(pool) == 0 {
		pool = official
	}
	sort.Slice(pool, func(i, j int) bool {
		ri, rj := pool[i].ref, pool[j].ref
		if ri.Name != rj.Name {
			return ri.Name < rj.Name
		}
		if ri.Channel == chartsrepo.DefaultChannel {
			return true
		}
		if rj.Channel == chartsrepo.DefaultChannel {
			return false
		}
		return ri.Channel < rj.Channel
	})
	return pool[0]
}

// scanRef reads every app under one source/channel.
func scanRef(ref SourceRef) []candidate {
	entries, err := os.ReadDir(filepath.Join(ref.Dir, "apps"))
	if err != nil {
		return nil
	}
	var out []candidate
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		app, err := loadApp(ref, entry.Name())
		if err != nil {
			continue
		}
		out = append(out, candidate{app: app, ref: ref})
	}
	return out
}

// loadApp parses and validates one app directory.
func loadApp(ref SourceRef, folder string) (*App, error) {
	data, err := os.ReadFile(filepath.Join(ref.Dir, "apps", folder, manifestFile))
	if err != nil {
		return nil, fmt.Errorf("reading %s: %w", manifestFile, err)
	}
	var m manifest
	if err := yaml.Unmarshal(data, &m); err != nil {
		return nil, fmt.Errorf("parsing %s: %w", manifestFile, err)
	}
	if err := validateManifest(&m, folder); err != nil {
		return nil, err
	}

	app := &App{
		Name:          m.Name,
		DisplayName:   displayOr(m.DisplayName, m.Name),
		Description:   m.Description,
		Category:      m.Category,
		Icon:          m.Icon,
		Website:       m.Website,
		Version:       m.Version,
		Tags:          normalizeList(m.Tags),
		DefaultValues: m.DefaultValues,
		Services:      normalizeServices(m.Services),
		Exposure:      m.Exposure,
		Repository:    ref.Name,
		Source:        ref.Name,
		Channel:       ref.Channel,
		ChartPath:     filepath.ToSlash(filepath.Join("apps", folder)),
		Chart:         folder,
	}
	if m.Schema != nil {
		raw, err := json.Marshal(m.Schema)
		if err != nil {
			return nil, fmt.Errorf("encoding schema for %q: %w", m.Name, err)
		}
		app.Schema = raw
	}
	if chartVersion := readChartVersion(filepath.Join(ref.Dir, "apps", folder)); chartVersion != "" {
		app.Version = chartVersion
	}
	app.Ports = servicePorts(app.Services)
	return app, nil
}

// readChartVersion returns Chart.yaml's version, which wins over the manifest's
// informational version.
func readChartVersion(dir string) string {
	data, err := os.ReadFile(filepath.Join(dir, chartFile))
	if err != nil {
		return ""
	}
	var meta struct {
		Version string `yaml:"version"`
	}
	if err := yaml.Unmarshal(data, &meta); err != nil {
		return ""
	}
	return meta.Version
}

func validateManifest(m *manifest, folder string) error {
	if m.Name == "" {
		return fmt.Errorf("app manifest is missing name")
	}
	if m.Name != folder {
		return fmt.Errorf("app name %q does not match folder %q", m.Name, folder)
	}
	if !dns1123Label.MatchString(m.Name) {
		return fmt.Errorf("app name %q must be a lowercase DNS-1123 label", m.Name)
	}
	for _, svc := range m.Services {
		if svc.Port <= 0 || svc.Port > 65535 {
			return fmt.Errorf("app %q service %q has invalid port %d", m.Name, svc.Name, svc.Port)
		}
		if svc.Scheme != "" && svc.Scheme != "http" && svc.Scheme != "https" {
			return fmt.Errorf("app %q service %q has invalid scheme %q", m.Name, svc.Name, svc.Scheme)
		}
		if !validServiceNameTemplate(svc.Name) {
			return fmt.Errorf("app %q service name %q may only be a literal or {{ .Release.Name }}", m.Name, svc.Name)
		}
	}
	return nil
}

// validServiceNameTemplate limits templating to the release name.
func validServiceNameTemplate(name string) bool {
	name = strings.TrimSpace(name)
	if name == "" {
		return false
	}
	if !strings.Contains(name, "{{") {
		return dns1123Label.MatchString(name)
	}
	remainder := strings.ReplaceAll(name, releaseNameTemplate, "")
	if strings.Contains(remainder, "{{") || strings.Contains(remainder, "}}") {
		return false
	}
	remainder = strings.TrimSpace(remainder)
	if remainder == "" {
		return true
	}
	return dns1123Label.MatchString(remainder)
}

func normalizeServices(in []Service) []Service {
	out := make([]Service, 0, len(in))
	for _, svc := range in {
		if svc.Scheme == "" {
			svc.Scheme = "http"
		}
		out = append(out, svc)
	}
	return out
}

func servicePorts(services []Service) []int {
	ports := make([]int, 0, len(services))
	for _, svc := range services {
		if svc.Port > 0 {
			ports = append(ports, svc.Port)
		}
	}
	return ports
}

func normalizeList(in []string) []string {
	out := make([]string, 0, len(in))
	seen := make(map[string]struct{}, len(in))
	for _, v := range in {
		v = strings.TrimSpace(v)
		if v == "" {
			continue
		}
		if _, dup := seen[v]; dup {
			continue
		}
		seen[v] = struct{}{}
		out = append(out, v)
	}
	return out
}

func displayOr(value, fallback string) string {
	if strings.TrimSpace(value) != "" {
		return value
	}
	return fallback
}

// List returns all catalog entries as a summary list.
func (c *Catalog) List() []CatalogEntry {
	entries := make([]CatalogEntry, 0, len(c.apps))
	for _, app := range c.apps {
		entries = append(entries, CatalogEntry{
			Name:        app.Name,
			DisplayName: app.DisplayName,
			Description: app.Description,
			Category:    app.Category,
			Icon:        app.Icon,
			Version:     app.Version,
			Tags:        app.Tags,
			Source:      app.Source,
			Channel:     app.Channel,
			Channels:    app.Channels,
			ChartPath:   app.ChartPath,
		})
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i].Name < entries[j].Name })
	return entries
}

// Get returns a full app definition by name.
//
// It returns a deep copy: callers merge install-time values into
// DefaultValues, and returning the shared entry let one request's merge leak
// into another request's view of the catalog (PF-M7).
func (c *Catalog) Get(name string) (*App, error) {
	app, ok := c.apps[name]
	if !ok {
		return nil, fmt.Errorf("app %q not found in catalog", name)
	}
	return cloneApp(app), nil
}

// Has reports whether an app is in the catalog.
func (c *Catalog) Has(name string) bool {
	_, ok := c.apps[name]
	return ok
}

// cloneApp deep-copies a catalog entry.
func cloneApp(app *App) *App {
	dup := *app
	if app.DefaultValues != nil {
		dup.DefaultValues = make(map[string]interface{}, len(app.DefaultValues))
		for k, v := range app.DefaultValues {
			dup.DefaultValues[k] = deepCopyValue(v)
		}
	}
	if app.Schema != nil {
		dup.Schema = append(json.RawMessage(nil), app.Schema...)
	}
	if app.Ports != nil {
		dup.Ports = append([]int(nil), app.Ports...)
	}
	if app.Tags != nil {
		dup.Tags = append([]string(nil), app.Tags...)
	}
	if app.Channels != nil {
		dup.Channels = append([]string(nil), app.Channels...)
	}
	if app.Services != nil {
		dup.Services = append([]Service(nil), app.Services...)
	}
	return &dup
}

// deepCopyValue copies the JSON-decoded containers that can appear in
// DefaultValues, so nested defaults are not shared either.
func deepCopyValue(v interface{}) interface{} {
	switch value := v.(type) {
	case map[string]interface{}:
		dup := make(map[string]interface{}, len(value))
		for k, item := range value {
			dup[k] = deepCopyValue(item)
		}
		return dup
	case []interface{}:
		dup := make([]interface{}, len(value))
		for i, item := range value {
			dup[i] = deepCopyValue(item)
		}
		return dup
	default:
		return value
	}
}

// Schema returns the JSON schema for an app's config form.
func (c *Catalog) Schema(name string) (json.RawMessage, error) {
	app, err := c.Get(name)
	if err != nil {
		return nil, err
	}
	return app.Schema, nil
}

// DefaultValues returns the default values for an app.
func (c *Catalog) DefaultValues(name string) (map[string]interface{}, error) {
	app, err := c.Get(name)
	if err != nil {
		return nil, err
	}
	return app.DefaultValues, nil
}
