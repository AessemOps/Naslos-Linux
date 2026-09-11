package server

import (
	"net/http"
	"strconv"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/metrics"
	"github.com/AessemOps/Naslos-Linux/api/internal/talos"
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

	data := s.metrics.Get()
	// The agent is the source of truth for pool state; overlay live pool
	// data so /api/metrics agrees with /api/dashboard.
	if pools := s.agentPools(); pools != nil {
		data.ZFS.Pools = pools
	}
	writeJSON(w, http.StatusOK, data)
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

	// The agent is the source of truth for pool state; overlay live pool
	// data so the dashboard always reflects the current pool state without
	// waiting for the next metrics collection cycle.
	if pools := s.agentPools(); pools != nil {
		data["zfs"] = map[string]interface{}{
			"poolCount": len(pools),
			"pools":     pools,
		}
	}

	writeJSON(w, http.StatusOK, data)
}

// convertNetworkInterfaces maps talos package types to the server metrics model.
func convertNetworkInterfaces(ifs []talos.NetworkInterface) []metrics.NetworkInterface {
	if ifs == nil {
		return nil
	}
	out := make([]metrics.NetworkInterface, 0, len(ifs))
	for _, i := range ifs {
		out = append(out, metrics.NetworkInterface{Name: i.Name, IPAddress: i.IPAddress})
	}
	return out
}

// agentPools fetches live ZFS pool data from the agent and converts it into
// dashboard-friendly pool metrics. Returns nil when the agent is unreachable,
// letting the caller fall back to the cached snapshot.
func (s *Server) agentPools() []metrics.PoolMetrics {
	pools, err := s.agent.ListPools()
	if err != nil {
		return nil
	}

	out := make([]metrics.PoolMetrics, 0, len(pools))
	for _, p := range pools {
		size := parseHumanSize(p.Size)
		alloc := parseHumanSize(p.Alloc)
		free := parseHumanSize(p.Free)
		usage := 0.0
		if size > 0 {
			usage = alloc / size * 100
		}
		out = append(out, metrics.PoolMetrics{
			Name:         p.Name,
			Size:         uint64(size),
			Alloc:        uint64(alloc),
			Free:         uint64(free),
			UsagePercent: usage,
			Health:       p.Health,
		})
	}
	return out
}
