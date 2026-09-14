package server

import (
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"os"
	"sort"
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

		// Refuse a path that is not on a ZFS dataset before anything is
		// created: this is what keeps share data inside the pool.
		if err := s.requireDatasetPath(req.Path); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if err := s.requireSharePathExist(req.Path); err != nil {
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
		// A share can be repointed at another folder; validate the new path
		// exactly like a create, so an edit cannot move data off the pool.
		if req.Path != nil {
			if err := s.requireDatasetPath(*req.Path); err != nil {
				writeError(w, http.StatusBadRequest, err.Error())
				return
			}
			if err := s.requireSharePathExist(*req.Path); err != nil {
				writeError(w, http.StatusBadRequest, err.Error())
				return
			}
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

// handleSharePaths lists the ZFS datasets that may be shared.
//
// Only real dataset mountpoints are offered: a directory that merely sits under
// the ZFS base (e.g. /var/mnt/tank when "tank" is not a dataset) lives on the
// node's EPHEMERAL partition, so offering it would invite a share whose data is
// outside the pool - no redundancy, no snapshots, and lost on an upgrade.
func (s *Server) handleSharePaths(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	mountpoints, err := s.datasetMountpoints()
	if err != nil {
		// Fail rather than fall back to listing the base directory: the
		// fallback is exactly how non-dataset paths used to be offered.
		writeError(w, http.StatusServiceUnavailable,
			"cannot list shareable datasets: "+err.Error())
		return
	}

	writeJSON(w, http.StatusOK, map[string]interface{}{
		"base":  s.shares.ZFSBase(),
		"paths": mountpoints,
	})
}

// handleShareFolders manages folders inside the share datasets, for building a
// share path that is a subfolder of a dataset ("link this share to a new folder
// in the pool").
//
//	GET    /api/shares/folders?path=/var/mnt/test       list subfolders
//	POST   /api/shares/folders  {"path":..., "name":...} create a subfolder
//	DELETE /api/shares/folders?path=/var/mnt/test/media  remove an empty folder
//
// Every path is checked against the node's datasets first, so a folder can only
// ever be created inside the pool - the same guarantee that keeps share data
// durable.
func (s *Server) handleShareFolders(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		path := r.URL.Query().Get("path")
		if err := s.requireDatasetPath(path); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		folders, err := s.agent.ListShareFolders(path)
		if err != nil {
			writeError(w, http.StatusBadGateway, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]interface{}{
			"base":    s.shares.ZFSBase(),
			"path":    path,
			"folders": folders,
		})

	case http.MethodPost:
		var req struct {
			Path string `json:"path"`
			Name string `json:"name"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if err := s.requireDatasetPath(req.Path); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		created, err := s.agent.CreateShareFolder(req.Path, req.Name)
		if err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusCreated, map[string]string{"path": created})

	case http.MethodDelete:
		path := r.URL.Query().Get("path")
		if err := s.requireDatasetPath(path); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if err := s.agent.DeleteShareFolder(path); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "folder removed", "path": path})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// requireSharePathExist confirms the share path exists as a folder before a
// share is pointed at it, so a typo is reported instead of a share that serves
// nothing.
//
// The API mounts the datasets read-only, which is enough to stat them. When that
// mount is absent (a deployment without api.datasetsHostPath) the check is
// skipped rather than blocking every share: the serving containers log what they
// cannot see either way.
func (s *Server) requireSharePathExist(path string) error {
	if _, err := os.Stat(s.shares.ZFSBase()); err != nil {
		return nil
	}
	return s.shares.ValidatePath(path)
}

// datasetMountpoints returns the mountpoints of all datasets on the node.
func (s *Server) datasetMountpoints() ([]string, error) {
	datasets, err := s.agent.ListDatasets()
	if err != nil {
		return nil, err
	}

	paths := make([]string, 0, len(datasets))
	for _, d := range datasets {
		if _, ok := shares.PathOnDataset(d.Mountpoint, []string{d.Mountpoint}); ok {
			paths = append(paths, d.Mountpoint)
		}
	}
	sort.Strings(paths)
	return paths, nil
}

// requireDatasetPath rejects a share path that is not on a ZFS dataset.
//
// Without this a share silently stores its data on the ephemeral partition,
// where it is not checksummed, not snapshotted, not redundant and is wiped by a
// Talos upgrade - the failure mode reported as "my files disappeared".
func (s *Server) requireDatasetPath(path string) error {
	datasets, err := s.agent.ListDatasets()
	if err != nil {
		return fmt.Errorf("cannot verify that %s is on a ZFS dataset (agent unavailable): %w", path, err)
	}

	mountpoints := make([]string, 0, len(datasets))
	for _, d := range datasets {
		mountpoints = append(mountpoints, d.Mountpoint)
	}

	if dataset, ok := shares.PathOnDataset(path, mountpoints); ok {
		_ = dataset
		return nil
	}

	return fmt.Errorf(
		"%s is not on a ZFS dataset, so its data would live on the node's ephemeral partition "+
			"(no snapshots, no redundancy, lost on upgrade). Create a dataset under one of %s first",
		path, strings.Join(mountpoints, ", "))
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

// ldapNSSGroups converts the LDAP groups into the form the extrausers group
// file needs, so Samba can resolve `valid users = @group` on the node.
//
// Membership is resolved from the group's member DNs (uid=<uid>,ou=people,...)
// down to plain uids, because NSS lists members by name. A group that cannot be
// read is skipped rather than failing the whole render: losing the share config
// would be worse than losing one group's membership, and the next apply
// (or LDAP recovery) restores it.
func (s *Server) ldapNSSGroups() []shares.NSSGroup {
	if s.identity == nil {
		return nil
	}

	groups, err := s.identity.ListGroups()
	if err != nil {
		log.Printf("Warning: could not read LDAP groups for share access lists: %v", err)
		return nil
	}

	out := make([]shares.NSSGroup, 0, len(groups))
	for _, g := range groups {
		members := make([]string, 0, len(g.Members))
		for _, dn := range g.Members {
			if uid := extractUID(dn); uid != "" {
				members = append(members, uid)
			}
		}
		out = append(out, shares.NSSGroup{
			Name:    g.CN,
			GID:     shares.GroupGID(g.CN),
			Members: members,
		})
	}
	return out
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
		bundle.NSSGroup = s.sambaUsers.RenderGroup(s.ldapNSSGroups())
		bundle.NSSShadow = s.sambaUsers.RenderShadow()
	}

	status, err := s.agent.ApplySharesConfig(agent.SharesConfigRequest{
		SambaConf:   bundle.SambaConf,
		GaneshaConf: bundle.GaneshaConf,
		SambaUsers:  bundle.SambaUsers,
		NSSPasswd:   bundle.NSSPasswd,
		NSSGroup:    bundle.NSSGroup,
		NSSShadow:   bundle.NSSShadow,
		Revision:    bundle.Revision,
		ShareCount:  bundle.ShareCount,
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

// handleNFSConfig returns the generated NFS-Ganesha configuration.
func (s *Server) handleNFSConfig(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	w.Header().Set("Content-Type", "text/plain")
	w.Write([]byte(s.shares.GenerateGaneshaConfig()))
}
