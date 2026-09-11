package server

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"regexp"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/agent"
)

// ZFS Pool API handlers.
// Pool operations are executed by the privileged naslos-agent DaemonSet
// running on each node, since ZFS pools live outside Talos's volume system.

// poolNamePattern is the ZFS-safe charset: must start alphanumerically.
var poolNamePattern = regexp.MustCompile(`^[a-zA-Z0-9][a-zA-Z0-9._:-]*$`)

// validTopologies are the topologies the agent's CreatePool understands.
var validTopologies = map[string]bool{
	"": true, "single": true, "mirror": true,
	"raidz": true, "raidz1": true, "raidz2": true, "raidz3": true,
}

func (s *Server) handleZFSPools(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		pools, err := s.agent.ListPools()
		if err != nil {
			writeAgentError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, pools)
	case http.MethodPost:
		// Create ZFS pool
		var req struct {
			Name     string            `json:"name"`
			Topology string            `json:"topology"` // mirror, raidz1, raidz2, raidz3
			Disks    []string          `json:"disks"`
			Options  map[string]string `json:"options"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		// Validate before forwarding so malformed requests fail fast with
		// 400 even when the agent is unreachable or degraded (503).
		if !poolNamePattern.MatchString(req.Name) {
			writeError(w, http.StatusBadRequest,
				fmt.Sprintf("invalid pool name %q: must start with a letter or digit and contain only letters, digits, '.', '_', ':' or '-'", req.Name))
			return
		}
		if len(req.Disks) == 0 {
			writeError(w, http.StatusBadRequest, "at least one disk is required")
			return
		}
		if !validTopologies[strings.ToLower(strings.TrimSpace(req.Topology))] {
			writeError(w, http.StatusBadRequest,
				fmt.Sprintf("unsupported topology %q (supported: single, mirror, raidz1, raidz2, raidz3)", req.Topology))
			return
		}
		if err := s.agent.CreatePool(agent.CreatePoolRequest{
			Name:     req.Name,
			Topology: req.Topology,
			Disks:    req.Disks,
			Options:  req.Options,
		}); err != nil {
			writeAgentError(w, err)
			return
		}
		writeJSON(w, http.StatusCreated, map[string]string{
			"status":   "pool created",
			"pool":     req.Name,
			"topology": req.Topology,
		})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleZFSPoolDetail handles per-pool operations: GET status, DELETE pool.
func (s *Server) handleZFSPoolDetail(w http.ResponseWriter, r *http.Request) {
	name := strings.TrimPrefix(r.URL.Path, "/api/volumes/zfs/")
	if name == "" || strings.Contains(name, "/") {
		writeError(w, http.StatusBadRequest, "pool name required")
		return
	}
	switch r.Method {
	case http.MethodGet:
		status, err := s.agent.PoolStatus(name)
		if err != nil {
			writeAgentError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"name": name, "status": status})
	case http.MethodDelete:
		if err := s.agent.DeletePool(name); err != nil {
			writeAgentError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "pool destroyed", "pool": name})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// writeAgentError maps agent client errors to HTTP responses. Agent-side
// failures (validation, degraded mode, zpool errors) carry their upstream
// status — notably 503 when the agent runs degraded (no ZFS on the host) —
// so the UI can distinguish "no ZFS here" from a real server error.
// Transport failures (agent unreachable) become 502.
func writeAgentError(w http.ResponseWriter, err error) {
	var agentErr *agent.Error
	if errors.As(err, &agentErr) && agentErr.Status != 0 {
		writeError(w, agentErr.Status, agentErr.Message)
		return
	}
	writeError(w, http.StatusBadGateway, err.Error())
}
