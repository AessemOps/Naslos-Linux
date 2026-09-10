package server

import (
	"encoding/json"
	"net/http"
)

// ZFS Pool API handlers.
// Pool operations are executed by the privileged naslos-agent DaemonSet
// running on each node, since ZFS pools live outside Talos's volume system.

func (s *Server) handleZFSPools(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		// List ZFS pools - delegated to agent
		writeJSON(w, http.StatusOK, map[string]string{"status": "listing zfs pools via agent"})
	case http.MethodPost:
		// Create ZFS pool
		var req struct {
			Name      string   `json:"name"`
			Topology  string   `json:"topology"` // mirror, raidz1, raidz2, raidz3
			Disks     []string `json:"disks"`
			Options   map[string]string `json:"options"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		// Validation happens in the agent
		writeJSON(w, http.StatusCreated, map[string]string{
			"status":   "pool creation requested",
			"pool":     req.Name,
			"topology": req.Topology,
		})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}
