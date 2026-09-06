package server

import (
	"encoding/json"
	"net/http"
)

// handleShares handles SMB/NFS/AFP share operations.
func (s *Server) handleShares(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		// List shares
		writeJSON(w, http.StatusOK, map[string]string{"status": "listing shares"})
	case http.MethodPost:
		// Create share
		var req struct {
			Name     string `json:"name"`
			Path     string `json:"path"`
			Protocol string `json:"protocol"` // smb, nfs, afp
			ReadOnly bool   `json:"readOnly"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusCreated, map[string]string{
			"status":   "share created",
			"name":     req.Name,
			"protocol": req.Protocol,
		})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleShareDetail handles individual share operations.
func (s *Server) handleShareDetail(w http.ResponseWriter, r *http.Request) {
	share := r.URL.Path[len("/api/shares/"):]
	switch r.Method {
	case http.MethodGet:
		writeJSON(w, http.StatusOK, map[string]string{"status": "getting share", "share": share})
	case http.MethodPut:
		writeJSON(w, http.StatusOK, map[string]string{"status": "updating share", "share": share})
	case http.MethodDelete:
		writeJSON(w, http.StatusOK, map[string]string{"status": "deleting share", "share": share})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}
