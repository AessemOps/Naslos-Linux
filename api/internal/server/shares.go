package server

import (
	"encoding/json"
	"net/http"

	"github.com/AessemOps/Naslos-Linux/api/internal/shares"
)

// handleShares handles share operations.
func (s *Server) handleShares(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		writeJSON(w, http.StatusOK, s.shares.List())

	case http.MethodPost:
		var req shares.CreateShareRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		share, err := s.shares.Create(req)
		if err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusCreated, share)

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleShareDetail handles individual share operations.
func (s *Server) handleShareDetail(w http.ResponseWriter, r *http.Request) {
	name := r.URL.Path[len("/api/shares/"):]

	switch r.Method {
	case http.MethodGet:
		share, err := s.shares.Get(name)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, share)

	case http.MethodPut:
		var req shares.UpdateShareRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		share, err := s.shares.Update(name, req)
		if err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, share)

	case http.MethodDelete:
		if err := s.shares.Delete(name); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "share deleted", "name": name})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleSambaConfig returns the generated Samba configuration.
func (s *Server) handleSambaConfig(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	w.Header().Set("Content-Type", "text/plain")
	w.Write([]byte(s.shares.GenerateSambaConfig()))
}

// handleNFSConfig returns the generated NFS exports configuration.
func (s *Server) handleNFSConfig(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	w.Header().Set("Content-Type", "text/plain")
	w.Write([]byte(s.shares.GenerateNFSExports()))
}
