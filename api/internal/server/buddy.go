package server

import (
	"encoding/json"
	"net/http"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
)

// handleBuddyStatus reports the receive side to the owner: how much room is left,
// which keys may push, and what is stored for each of them. It never exposes a
// backup's contents - there is nothing here to expose, and that is the point.
func (s *Server) handleBuddyStatus(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if s.buddy == nil {
		writeError(w, http.StatusServiceUnavailable, "Buddy Backup is not configured on this instance")
		return
	}

	free, err := s.buddy.Store.FreeSpace()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "cannot read free space: "+err.Error())
		return
	}
	used, err := s.buddy.Store.Usage("")
	if err != nil {
		writeError(w, http.StatusInternalServerError, "cannot read usage: "+err.Error())
		return
	}
	backups, err := s.buddy.Store.Summary("")
	if err != nil {
		writeError(w, http.StatusInternalServerError, "cannot list backups: "+err.Error())
		return
	}

	peers := s.buddy.Peers.List()
	if peers == nil {
		peers = []*buddy.Peer{}
	}
	if backups == nil {
		backups = []buddy.Backups{}
	}

	writeJSON(w, http.StatusOK, map[string]any{
		"enabled":        true,
		"name":           s.buddy.Name,
		"version":        s.buddy.Version,
		"receivePath":    s.buddy.Store.Root(),
		"freeBytes":      free,
		"usedBytes":      used,
		"enrollmentOpen": s.buddy.EnrollToken != "",
		"peers":          peers,
		"backups":        backups,
	})
}

// handleBuddyPeers authorizes or revokes a peer key by hand, for operators who
// prefer to keep a shared enrollment token out of the picture.
func (s *Server) handleBuddyPeers(w http.ResponseWriter, req *http.Request) {
	if s.buddy == nil {
		writeError(w, http.StatusServiceUnavailable, "Buddy Backup is not configured on this instance")
		return
	}

	switch req.Method {
	case http.MethodGet:
		peers := s.buddy.Peers.List()
		if peers == nil {
			peers = []*buddy.Peer{}
		}
		writeJSON(w, http.StatusOK, map[string]any{"peers": peers})

	case http.MethodPost:
		var request struct {
			Name           string   `json:"name"`
			PublicKey      string   `json:"publicKey"`
			AllowedSources []string `json:"allowedSources"`
			QuotaBytes     int64    `json:"quotaBytes"`
			Enabled        *bool    `json:"enabled"`
		}
		if err := json.NewDecoder(http.MaxBytesReader(w, req.Body, 64<<10)).Decode(&request); err != nil {
			writeError(w, http.StatusBadRequest, "invalid request: "+err.Error())
			return
		}
		if strings.TrimSpace(request.Name) == "" || strings.TrimSpace(request.PublicKey) == "" {
			writeError(w, http.StatusBadRequest, "name and publicKey are required")
			return
		}

		// Updating an existing peer keeps its stored permissions unless the
		// caller spelled them out, so a rename cannot silently widen access.
		peer := &buddy.Peer{
			Name:           request.Name,
			PublicKey:      request.PublicKey,
			AllowedSources: request.AllowedSources,
			QuotaBytes:     request.QuotaBytes,
			Enabled:        true,
		}
		if existing := s.buddy.Peers.Get(request.Name); existing != nil {
			peer.CreatedAt = existing.CreatedAt
			if request.AllowedSources == nil {
				peer.AllowedSources = existing.AllowedSources
			}
			if request.QuotaBytes == 0 {
				peer.QuotaBytes = existing.QuotaBytes
			}
		}
		if request.Enabled != nil {
			peer.Enabled = *request.Enabled
		}
		if err := s.buddy.Peers.Add(peer); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]any{"peer": peer})

	case http.MethodDelete:
		name := strings.TrimSpace(req.URL.Query().Get("name"))
		if name == "" {
			writeError(w, http.StatusBadRequest, "name is required")
			return
		}
		if err := s.buddy.Peers.Remove(name); err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{
			"status": "revoked",
			"name":   name,
			"note":   "chunks already received stay until they are pruned: revoking a key stops new pushes, it is not a delete of existing backups",
		})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}
