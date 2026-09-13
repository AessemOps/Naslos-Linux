package server

import (
	"encoding/json"
	"log"
	"net/http"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/agent"
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
		s.applySharesConfig()
		writeJSON(w, http.StatusCreated, share)

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleShareDetail handles individual share operations.
func (s *Server) handleShareDetail(w http.ResponseWriter, r *http.Request) {
	name := strings.TrimPrefix(r.URL.Path, "/api/shares/")
	if name == "" {
		writeError(w, http.StatusBadRequest, "share name is required")
		return
	}

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
		s.applySharesConfig()
		writeJSON(w, http.StatusOK, share)

	case http.MethodDelete:
		if err := s.shares.Delete(name); err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		s.applySharesConfig()
		writeJSON(w, http.StatusOK, map[string]string{"status": "share deleted", "name": name})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleSharePaths lists the ZFS dataset directories that may be shared.
// Returning a guaranteed (possibly empty) array keeps the create form usable
// even when no datasets exist yet, instead of leaving its path picker blank.
func (s *Server) handleSharePaths(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	paths, err := s.shares.AvailablePaths()
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, map[string]interface{}{
		"base":  s.shares.ZFSBase(),
		"paths": paths,
	})
}

// handleSharesStatus reports whether the node-side share services have the
// current configuration applied.
func (s *Server) handleSharesStatus(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	bundle := s.shares.RenderConfigBundle()

	response := map[string]interface{}{
		"revision":   bundle.Revision,
		"shareCount": bundle.ShareCount,
		"agent":      nil,
		"agentError": "",
	}

	status, err := s.agent.GetSharesStatus()
	if err != nil {
		response["agentError"] = err.Error()
	} else {
		response["agent"] = status
	}

	writeJSON(w, http.StatusOK, response)
}

// handleSharesApply re-renders the share configuration and pushes it to the
// privileged agent, which writes it for the node's share services.
func (s *Server) handleSharesApply(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost && r.Method != http.MethodPut {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	status, err := s.applySharesConfig()
	if err != nil {
		writeError(w, http.StatusBadGateway, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, status)
}

// applySharesConfig renders the current share configuration and pushes it to
// the agent. Failures are logged and surfaced to the caller but never roll
// back the share definition: the API remains the source of truth and the next
// apply (or API restart) converges the node.
func (s *Server) applySharesConfig() (*agent.SharesConfigStatus, error) {
	bundle := s.shares.RenderConfigBundle()

	// Attach the SMB account mirror so LDAP password changes reach Samba's
	// passdb in the same push as the share configuration.
	if s.sambaUsers != nil {
		bundle.SambaUsers = s.sambaUsers.RenderSMBPasswd()
		bundle.NSSPasswd = s.sambaUsers.RenderPasswd()
		bundle.NSSGroup = s.sambaUsers.RenderGroup()
		bundle.NSSShadow = s.sambaUsers.RenderShadow()
	}

	status, err := s.agent.ApplySharesConfig(agent.SharesConfigRequest{
		SambaConf:  bundle.SambaConf,
		NFSExports: bundle.NFSExports,
		SambaUsers: bundle.SambaUsers,
		NSSPasswd:  bundle.NSSPasswd,
		NSSGroup:   bundle.NSSGroup,
		NSSShadow:  bundle.NSSShadow,
		Revision:   bundle.Revision,
		ShareCount: bundle.ShareCount,
	})
	if err != nil {
		log.Printf("Warning: failed to apply shares config (revision %s): %v", bundle.Revision, err)
		return nil, err
	}
	return status, nil
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
