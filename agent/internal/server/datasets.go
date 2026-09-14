// Dataset listing endpoint. The handler for per-pool dataset operations lives
// in server.go; this one reports every dataset so the API can tell whether a
// share path is genuinely ZFS-backed rather than a directory on the ephemeral
// partition.
package server

import (
	"net/http"
)

// handleDatasetsAll lists every dataset on the node with its mountpoint. Used
// to decide whether a share path is genuinely ZFS-backed.
func (s *Server) handleDatasetsAll(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if s.zfsUnavailable(w) {
		return
	}

	datasets, err := s.zfs.AllDatasets()
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, datasets)
}

// handleDatasets handles dataset operations for a pool.
