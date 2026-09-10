package catalog

import "encoding/json"

// loadBuiltIn loads the built-in app catalog.
func (c *Catalog) loadBuiltIn() {
	// Plex Media Server
	c.apps["plex"] = &App{
		Name:        "plex",
		DisplayName: "Plex Media Server",
		Description: "Stream your media collection to any device. Organize movies, TV shows, music, and photos.",
		Category:    "media",
		Icon:        "🎬",
		Version:     "1.41.0",
		Chart:       "plexinc/pms-docker",
		Repository:  "https://charts.plex.tv",
		Tags:        []string{"media", "streaming", "video", "music"},
		Ports:       []int{32400},
		Website:     "https://www.plex.tv",
		Schema: json.RawMessage(`{
			"type": "object",
			"properties": {
				"claimToken": {"type": "string", "title": "Plex Claim Token", "description": "Get from https://www.plex.tv/claim"},
				"timezone": {"type": "string", "title": "Timezone", "default": "UTC"},
				"advertiseIp": {"type": "string", "title": "Advertise IP", "default": "http://localhost:32400"},
				"transcoder": {
					"type": "object",
					"title": "Transcoder",
					"properties": {
						"tempPath": {"type": "string", "title": "Temp Path", "default": "/transcode"},
						"hardwareAcceleration": {"type": "boolean", "title": "Hardware Acceleration", "default": true}
					}
				}
			},
			"required": ["claimToken"]
		}`),
		DefaultValues: map[string]interface{}{
			"timezone":    "UTC",
			"advertiseIp": "http://localhost:32400",
			"transcoder": map[string]interface{}{
				"tempPath":              "/transcode",
				"hardwareAcceleration": true,
			},
		},
	}

	// Nextcloud
	c.apps["nextcloud"] = &App{
		Name:        "nextcloud",
		DisplayName: "Nextcloud",
		Description: "Self-hosted file sync and share platform. Calendar, contacts, documents, and more.",
		Category:    "productivity",
		Icon:        "☁️",
		Version:     "30.0.0",
		Chart:       "nextcloud/nextcloud",
		Repository:  "https://nextcloud.github.io/helm",
		Tags:        []string{"files", "sync", "productivity", "calendar"},
		Ports:       []int{80},
		Website:     "https://nextcloud.com",
		Schema: json.RawMessage(`{
			"type": "object",
			"properties": {
				"adminUsername": {"type": "string", "title": "Admin Username", "default": "admin"},
				"adminPassword": {"type": "string", "title": "Admin Password", "format": "password"},
				"domain": {"type": "string", "title": "Domain", "description": "Your Nextcloud domain"},
				"storageClass": {"type": "string", "title": "Storage Class", "default": "naslos-zfs"},
				"storageSize": {"type": "string", "title": "Storage Size", "default": "100Gi"}
			},
			"required": ["adminPassword"]
		}`),
		DefaultValues: map[string]interface{}{
			"adminUsername": "admin",
			"storageClass":  "naslos-zfs",
			"storageSize":   "100Gi",
		},
	}

	// Jellyfin
	c.apps["jellyfin"] = &App{
		Name:        "jellyfin",
		DisplayName: "Jellyfin",
		Description: "Free media system. No fees, no tracking, no central server. Your media, your server.",
		Category:    "media",
		Icon:        "📺",
		Version:     "10.9.0",
		Chart:       "jellyfin/jellyfin",
		Repository:  "https://jellyfin.github.io/repo",
		Tags:        []string{"media", "streaming", "video", "free"},
		Ports:       []int{8096},
		Website:     "https://jellyfin.org",
		Schema: json.RawMessage(`{
			"type": "object",
			"properties": {
				"timezone": {"type": "string", "title": "Timezone", "default": "UTC"},
				"cachePath": {"type": "string", "title": "Cache Path", "default": "/cache"},
				"mediaPath": {"type": "string", "title": "Media Path", "default": "/media"},
				"hardwareAcceleration": {"type": "boolean", "title": "Hardware Acceleration", "default": true}
			}
		}`),
		DefaultValues: map[string]interface{}{
			"timezone":             "UTC",
			"cachePath":            "/cache",
			"mediaPath":            "/media",
			"hardwareAcceleration": true,
		},
	}
}
