package catalog

import "encoding/json"

// loadBuiltInExtra loads additional built-in apps.
func (c *Catalog) loadBuiltInExtra() {
	// Home Assistant
	c.apps["homeassistant"] = &App{
		Name:        "homeassistant",
		DisplayName: "Home Assistant",
		Description: "Open-source home automation. Control all your devices from a single interface.",
		Category:    "smart-home",
		Icon:        "🏠",
		Version:     "2024.12",
		Chart:       "home-assistant/home-assistant",
		Repository:  "https://home-assistant.github.io/home-assistant",
		Tags:        []string{"automation", "iot", "smart-home"},
		Ports:       []int{8123},
		Website:     "https://www.home-assistant.io",
		Schema: json.RawMessage(`{
			"type": "object",
			"properties": {
				"timezone": {"type": "string", "title": "Timezone", "default": "UTC"},
				"configPath": {"type": "string", "title": "Config Path", "default": "/config"},
				"ingress": {
					"type": "object",
					"title": "Ingress",
					"properties": {
						"enabled": {"type": "boolean", "title": "Enable Ingress", "default": true},
						"hostname": {"type": "string", "title": "Hostname"}
					}
				}
			}
		}`),
		DefaultValues: map[string]interface{}{
			"timezone":   "UTC",
			"configPath": "/config",
			"ingress": map[string]interface{}{
				"enabled": true,
			},
		},
	}

	// Pi-hole
	c.apps["pihole"] = &App{
		Name:        "pihole",
		DisplayName: "Pi-hole",
		Description: "Network-wide ad blocking. Block ads on every device without client software.",
		Category:    "networking",
		Icon:        "🛡️",
		Version:     "2024.02",
		Chart:       "pihole/pihole",
		Repository:  "https://github.com/MoJo2600/pihole-kubernetes",
		Tags:        []string{"dns", "ad-block", "networking", "privacy"},
		Ports:       []int{80, 53},
		Website:     "https://pi-hole.net",
		Schema: json.RawMessage(`{
			"type": "object",
			"properties": {
				"password": {"type": "string", "title": "Admin Password", "format": "password"},
				"dns1": {"type": "string", "title": "Upstream DNS 1", "default": "1.1.1.1"},
				"dns2": {"type": "string", "title": "Upstream DNS 2", "default": "8.8.8.8"},
				"timezone": {"type": "string", "title": "Timezone", "default": "UTC"},
				"dhcp": {"type": "boolean", "title": "Enable DHCP", "default": false}
			},
			"required": ["password"]
		}`),
		DefaultValues: map[string]interface{}{
			"dns1":     "1.1.1.1",
			"dns2":     "8.8.8.8",
			"timezone": "UTC",
			"dhcp":     false,
		},
	}

	// Gitea
	c.apps["gitea"] = &App{
		Name:        "gitea",
		DisplayName: "Gitea",
		Description: "Self-hosted Git service. Lightweight, fast, and easy to install.",
		Category:    "development",
		Icon:        "🔧",
		Version:     "1.22.0",
		Chart:       "gitea/gitea",
		Repository:  "https://dl.gitea.io/charts",
		Tags:        []string{"git", "development", "code"},
		Ports:       []int{3000, 22},
		Website:     "https://gitea.io",
		Schema: json.RawMessage(`{
			"type": "object",
			"properties": {
				"adminUsername": {"type": "string", "title": "Admin Username", "default": "gitea_admin"},
				"adminPassword": {"type": "string", "title": "Admin Password", "format": "password"},
				"adminEmail": {"type": "string", "title": "Admin Email", "format": "email"},
				"domain": {"type": "string", "title": "Domain"},
				"sshEnabled": {"type": "boolean", "title": "Enable SSH", "default": true}
			},
			"required": ["adminPassword"]
		}`),
		DefaultValues: map[string]interface{}{
			"adminUsername": "gitea_admin",
			"sshEnabled":    true,
		},
	}

	// Syncthing
	c.apps["syncthing"] = &App{
		Name:        "syncthing",
		DisplayName: "Syncthing",
		Description: "Continuous file synchronization. Sync files between your devices securely.",
		Category:    "productivity",
		Icon:        "🔄",
		Version:     "1.27.0",
		Chart:       "syncthing/syncthing",
		Repository:  "https://syncthing.net",
		Tags:        []string{"sync", "files", "privacy"},
		Ports:       []int{8384, 22000},
		Website:     "https://syncthing.net",
		Schema: json.RawMessage(`{
			"type": "object",
			"properties": {
				"guiUsername": {"type": "string", "title": "GUI Username", "default": "admin"},
				"guiPassword": {"type": "string", "title": "GUI Password", "format": "password"},
				"syncPath": {"type": "string", "title": "Sync Path", "default": "/sync"}
			}
		}`),
		DefaultValues: map[string]interface{}{
			"guiUsername": "admin",
			"syncPath":    "/sync",
		},
	}

	// Immich
	c.apps["immich"] = &App{
		Name:        "immich",
		DisplayName: "Immich",
		Description: "Self-hosted photo and video backup solution. Google Photos alternative.",
		Category:    "media",
		Icon:        "📸",
		Version:     "1.121.0",
		Chart:       "immich/immich",
		Repository:  "https://immich.app",
		Tags:        []string{"photos", "backup", "media"},
		Ports:       []int{2283},
		Website:     "https://immich.app",
		Schema: json.RawMessage(`{
			"type": "object",
			"properties": {
				"adminEmail": {"type": "string", "title": "Admin Email", "format": "email"},
				"adminPassword": {"type": "string", "title": "Admin Password", "format": "password"},
				"libraryPath": {"type": "string", "title": "Library Path", "default": "/photos"},
				"machineLearning": {"type": "boolean", "title": "Enable ML", "default": true}
			},
			"required": ["adminEmail", "adminPassword"]
		}`),
		DefaultValues: map[string]interface{}{
			"libraryPath":     "/photos",
			"machineLearning": true,
		},
	}
}
