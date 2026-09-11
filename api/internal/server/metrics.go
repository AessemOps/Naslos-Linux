package server

import (
	"net/http"
	"strconv"
	"strings"
)

// parseHumanSize converts a zpool-style human-readable size ("19.5G", "468K")
// into bytes. Returns 0 for unparseable values so the dashboard degrades
// gracefully rather than breaking the whole endpoint.
func parseHumanSize(s string) float64 {
	s = strings.TrimSpace(s)
	if s == "" {
		return 0
	}
	// Extract the numeric suffix (K, M, G, T, P) if any.
	mult := 1.0
	u := strings.ToUpper(s)
	switch {
	case strings.HasSuffix(u, "K"):
		mult = 1024
		s = s[:len(s)-1]
	case strings.HasSuffix(u, "M"):
		mult = 1024 * 1024
		s = s[:len(s)-1]
	case strings.HasSuffix(u, "G"):
		mult = 1024 * 1024 * 1024
		s = s[:len(s)-1]
	case strings.HasSuffix(u, "T"):
		mult = 1024 * 1024 * 1024 * 1024
		s = s[:len(s)-1]
	case strings.HasSuffix(u, "P"):
		mult = 1024 * 1024 * 1024 * 1024 * 1024
		s = s[:len(s)-1]
	}
	n, err := strconv.ParseFloat(strings.TrimSpace(s), 64)
	if err != nil {
		return 0
	}
	return n * mult
}

// handleMetrics returns system metrics for the dashboard.
func (s *Server) handleMetrics(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	writeJSON(w, http.StatusOK, s.metrics.Get())
}

// handleDashboard returns dashboard-formatted metrics for the home screen.
// It augments the cached metrics with live ZFS pool data from the agent so the
// dashboard always reflects the current pool state without waiting for the
// next metrics collection cycle.
func (s *Server) handleDashboard(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	data := s.metrics.GetDashboardData()

	// Fetch live pool data from the agent and inject it. The agent is the
	// source of truth for pool state; the metrics manager only holds a
	// snapshot that is never updated by any collector.
	if pools, err := s.agent.ListPools(); err == nil {
		poolMaps := make([]map[string]interface{}, 0, len(pools))
		for _, p := range pools {
			size := parseHumanSize(p.Size)
			alloc := parseHumanSize(p.Alloc)
			free := parseHumanSize(p.Free)
			usage := 0.0
			if size > 0 {
				usage = alloc / size * 100
			}
			poolMaps = append(poolMaps, map[string]interface{}{
				"name":         p.Name,
				"size":         size,
				"alloc":        alloc,
				"free":         free,
				"usagePercent": usage,
				"health":       p.Health,
			})
		}
		data["zfs"] = map[string]interface{}{
			"poolCount": len(poolMaps),
			"pools":     poolMaps,
		}
	}

	writeJSON(w, http.StatusOK, data)
}
