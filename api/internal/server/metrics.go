package server

import (
	"net/http"
)

// handleMetrics returns system metrics for the dashboard.
func (s *Server) handleMetrics(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	// In production: query Prometheus for cluster/node/ZFS metrics
	writeJSON(w, http.StatusOK, map[string]interface{}{
		"status": "metrics endpoint",
		"sources": []string{
			"prometheus",
			"node-exporter",
			"zfs_exporter",
			"talos-metrics",
		},
	})
}
