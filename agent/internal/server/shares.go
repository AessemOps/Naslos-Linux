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
		SambaConf:   req.SambaConf,
		GaneshaConf: req.GaneshaConf,
		SambaUsers:  req.SambaUsers,
		NSSPasswd:   req.NSSPasswd,
		NSSGroup:    req.NSSGroup,
		NSSShadow:   req.NSSShadow,
		Revision:    req.Revision,
		ShareCount:  req.ShareCount,
	})
	if err != nil {
		writeClientError(w, err)
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
		writeClientError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, status)
}

// handleShareFolders lists, creates and removes folders in the share datasets.
//
// The API cannot do this itself: it mounts the datasets read-only (it only needs
// to list and validate paths). The privileged agent owns every write to the
// host, so folder management lives here.
//
//	GET    /api/v1/shares/folders?path=/var/mnt/test
//	POST   /api/v1/shares/folders  {"path":"/var/mnt/test","name":"media"}
//	DELETE /api/v1/shares/folders?path=/var/mnt/test/media   (empty folders only)
func (s *Server) handleShareFolders(w http.ResponseWriter, r *http.Request) {
	if s.shares == nil {
		writeError(w, http.StatusServiceUnavailable,
			"share folders are not available on this node (agent running in degraded mode)")
		return
	}

	switch r.Method {
	case http.MethodGet:
		path := r.URL.Query().Get("path")
		folders, err := s.shares.ListFolders(path)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]interface{}{
			"path":    path,
			"folders": folders,
		})

	case http.MethodPost:
		var req shareFolderRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		created, err := s.shares.CreateFolder(req.Path, req.Name)
		if err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		log.Printf("created share folder %s", created)
		writeJSON(w, http.StatusCreated, map[string]string{"path": created})

	case http.MethodDelete:
		path := r.URL.Query().Get("path")
		if err := s.shares.DeleteFolder(path); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		log.Printf("removed share folder %s", path)
		writeJSON(w, http.StatusOK, map[string]string{"status": "folder removed", "path": path})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// shareFolderRequest is the body of a folder-create request. path is the parent
// folder that already exists.
type shareFolderRequest struct {
	Path string `json:"path"`
	Name string `json:"name"`
}

// sharesConfigRequest mirrors the API's rendered-config payload. Declared
// locally so the agent keeps its HTTP contract independent of the API types.
type sharesConfigRequest struct {
	SambaConf   string `json:"sambaConf"`
	GaneshaConf string `json:"ganeshaConf"`
	// SambaUsers is the smbpasswd-format account file whose NT hashes are
	// imported into Samba's passdb.
	SambaUsers string `json:"sambaUsers"`
	// NSSPasswd / NSSGroup / NSSShadow are extrausers-format files that make
	// the container able to resolve LDAP users through NSS.
	NSSPasswd  string `json:"nssPasswd"`
	NSSGroup   string `json:"nssGroup"`
	NSSShadow  string `json:"nssShadow"`
	Revision   string `json:"revision"`
	ShareCount int    `json:"shareCount"`
}
