// Share configuration endpoints. The agent is the only component that can
// write to the Talos host filesystem, so the API renders smb.conf/exports and
// pushes them here for atomic writes into /var/lib/naslos/shares.
package server

import (
	"encoding/json"
	"log"
	"net/http"

	"github.com/AessemOps/Naslos-Linux/agent/internal/shares"
)

// zfsUnavailable lives in server.go; the handlers below cover share config,
// which is independent of ZFS availability.

func (s *Server) handleSharesConfig(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPut && r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if s.shares == nil {
		writeError(w, http.StatusServiceUnavailable,
			"share configuration is not available on this node (agent running in degraded mode)")
		return
	}

	var req sharesConfigRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	status, err := s.shares.Apply(shares.Config{
		SambaConf:  req.SambaConf,
		NFSExports: req.NFSExports,
		SambaUsers: req.SambaUsers,
		Revision:   req.Revision,
		ShareCount: req.ShareCount,
	})
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}

	log.Printf("applied shares config revision %s (%d enabled shares, %d smb sections, %d nfs exports)",
		req.Revision, req.ShareCount, status.SMBShareCount, status.NFSExportCount)

	writeJSON(w, http.StatusOK, status)
}

// handleSharesStatus reports the share configuration present on the host.
func (s *Server) handleSharesStatus(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if s.shares == nil {
		writeError(w, http.StatusServiceUnavailable,
			"share configuration is not available on this node (agent running in degraded mode)")
		return
	}

	status, err := s.shares.Status()
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, status)
}

// sharesConfigRequest mirrors the API's rendered-config payload. Declared
// locally so the agent keeps its HTTP contract independent of the API types.
type sharesConfigRequest struct {
	SambaConf  string `json:"sambaConf"`
	NFSExports string `json:"nfsExports"`
	// SambaUsers is the smbpasswd-format account file whose NT hashes are
	// imported into Samba's passdb.
	SambaUsers string `json:"sambaUsers"`
	Revision   string `json:"revision"`
	ShareCount int    `json:"shareCount"`
}
