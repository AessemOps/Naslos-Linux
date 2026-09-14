// Package catalog provides the Naslos app catalog — curated apps with
// JSON schemas that generate config forms in the UI.
package catalog

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// Catalog is the app catalog.
type Catalog struct {
	apps        map[string]*App
	catalogPath string
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
	Schema        json.RawMessage        `json:"schema"` // JSON Schema for config form
	DefaultValues map[string]interface{} `json:"defaultValues"`
	Ports         []int                  `json:"ports"`
	Website       string                 `json:"website"`
	Tags          []string               `json:"tags"`
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
}

// New creates a new catalog from a directory of app definitions.
func New(catalogPath string) *Catalog {
	c := &Catalog{
		apps:        make(map[string]*App),
		catalogPath: catalogPath,
	}
	c.load()
	return c
}

// load loads all app definitions from the catalog directory.
func (c *Catalog) load() {
	// Always load built-in apps first
	c.loadBuiltIn()
	c.loadBuiltInExtra()

	if c.catalogPath == "" {
		return
	}

	entries, err := os.ReadDir(c.catalogPath)
	if err != nil {
		return
	}

	for _, entry := range entries {
		if entry.IsDir() || filepath.Ext(entry.Name()) != ".json" {
			continue
		}

		data, err := os.ReadFile(filepath.Join(c.catalogPath, entry.Name()))
		if err != nil {
			continue
		}

		var app App
		if err := json.Unmarshal(data, &app); err != nil {
			continue
		}

		// User catalog can override built-in
		c.apps[app.Name] = &app
	}
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
		})
	}
	return entries
}

// Get returns a full app definition by name.
func (c *Catalog) Get(name string) (*App, error) {
	app, ok := c.apps[name]
	if !ok {
		return nil, fmt.Errorf("app %q not found in catalog", name)
	}
	return app, nil
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
