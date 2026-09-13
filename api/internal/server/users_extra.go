package server

import (
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/shares"
)

// handleUserPassword handles password changes.
func (s *Server) handleUserPassword(w http.ResponseWriter, r *http.Request) {
	if s.identityUnavailable(w) {
		return
	}
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	path := strings.TrimPrefix(r.URL.Path, "/api/users/")
	uid := strings.TrimSuffix(path, "/password")

	var req struct {
		Password string `json:"password"`
	}
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	if req.Password == "" {
		writeError(w, http.StatusBadRequest, "password is required")
		return
	}

	ntHash, err := s.identity.SetPassword(uid, req.Password)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}

	if err := s.syncSMBPassword(uid, ntHash); err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}

	writeJSON(w, http.StatusOK, map[string]string{"status": "password updated"})
}

// handleUserEnable handles enable/disable operations.
func (s *Server) handleUserEnable(w http.ResponseWriter, r *http.Request) {
	if s.identityUnavailable(w) {
		return
	}
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	path := strings.TrimPrefix(r.URL.Path, "/api/users/")
	parts := strings.Split(path, "/")
	if len(parts) != 2 {
		writeError(w, http.StatusBadRequest, "invalid path")
		return
	}
	uid, action := parts[0], parts[1]

	var err error
	enabled := false
	switch action {
	case "enable":
		enabled = true
		err = s.identity.EnablePerson(uid)
	case "disable":
		enabled = false
		err = s.identity.DisablePerson(uid)
	default:
		writeError(w, http.StatusBadRequest, "invalid action")
		return
	}

	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}

	// Keep SMB access in step with the LDAP account state: a disabled user
	// must not keep authenticating over SMB.
	if err := s.setSMBUserEnabled(uid, enabled); err != nil {
		log.Printf("Warning: could not mirror SMB account state for %s: %v", uid, err)
	}

	writeJSON(w, http.StatusOK, map[string]string{"status": "user " + action + "d"})
}

// handleGroups handles group operations.
func (s *Server) handleGroups(w http.ResponseWriter, r *http.Request) {
	if s.identityUnavailable(w) {
		return
	}
	switch r.Method {
	case http.MethodGet:
		groups, err := s.identity.ListGroups()
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, groups)

	case http.MethodPost:
		var req struct {
			CN          string `json:"cn"`
			Description string `json:"description"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		group, err := s.identity.CreateGroup(req.CN, req.Description)
		if err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusCreated, group)

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleGroupDetail handles individual group operations.
func (s *Server) handleGroupDetail(w http.ResponseWriter, r *http.Request) {
	if s.identityUnavailable(w) {
		return
	}
	cn := r.URL.Path[len("/api/groups/"):]

	switch r.Method {
	case http.MethodGet:
		group, err := s.identity.GetGroup(cn)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, group)

	case http.MethodPut:
		var req struct {
			Members []string `json:"members"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		group, err := s.identity.GetGroup(cn)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}

		for _, member := range group.Members {
			if !contains(req.Members, member) {
				s.identity.RemoveMember(cn, extractUID(member))
			}
		}
		for _, member := range req.Members {
			if !contains(group.Members, member) {
				s.identity.AddMember(cn, member)
			}
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "group updated"})

	case http.MethodDelete:
		if err := s.identity.DeleteGroup(cn); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "group deleted"})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleAuthMe returns the current authenticated user.
func (s *Server) handleAuthMe(w http.ResponseWriter, r *http.Request) {
	if s.identityUnavailable(w) {
		return
	}
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	username := r.Header.Get("Remote-User")
	if username == "" {
		writeError(w, http.StatusUnauthorized, "not authenticated")
		return
	}

	writeJSON(w, http.StatusOK, map[string]interface{}{
		"username":    username,
		"groups":      r.Header.Get("Remote-Groups"),
		"email":       r.Header.Get("Remote-Email"),
		"displayName": r.Header.Get("Remote-Name"),
	})
}

func contains(slice []string, item string) bool {
	for _, s := range slice {
		if s == item {
			return true
		}
	}
	return false
}

func extractUID(dn string) string {
	if strings.HasPrefix(dn, "uid=") {
		parts := strings.Split(dn, ",")
		return strings.TrimPrefix(parts[0], "uid=")
	}
	return dn
}

// Store accessor for the SMB account mirror. The store is created alongside
// the share manager; tests may construct a Server without one.
func (s *Server) smbUsers() *shares.SambaUserStore {
	return s.sambaUsers
}

// syncSMBPassword records the user's NT hash so SMB logins use the same
// password as the web UI, persists it, and pushes the account file to the
// node. The POSIX uid is read from LDAP so Samba and LDAP agree on the
// account identity; if it cannot be read the account is still recorded and
// the uid falls back to a Samba-assigned one.
func (s *Server) syncSMBPassword(uid, ntHash string) error {
	if s.sambaUsers == nil {
		return fmt.Errorf("SMB account store is not available")
	}
	if uid == "" || ntHash == "" {
		return fmt.Errorf("uid and NT hash are required to sync an SMB account")
	}

	uidNumber := 0
	if s.identity != nil {
		if n, err := s.identity.GetUIDNumber(uid); err == nil {
			uidNumber = n
		} else {
			log.Printf("Warning: could not read uidNumber for %s (SMB account will use Samba's own uid): %v", uid, err)
		}
	}

	if err := s.sambaUsers.Upsert(uid, uidNumber, ntHash); err != nil {
		return err
	}

	// Push immediately so the change takes effect without waiting for the
	// next share edit; a failure here is reported but must not lose the
	// stored hash (the next apply converges the node).
	if _, err := s.applySharesConfig(); err != nil {
		return fmt.Errorf("SMB account recorded but pushing it to the node failed: %w", err)
	}
	return nil
}

// removeSMBUser drops the SMB account mirroring a deleted LDAP user.
func (s *Server) removeSMBUser(uid string) error {
	if s.sambaUsers == nil {
		return nil
	}
	if err := s.sambaUsers.Remove(uid); err != nil {
		return err
	}
	if _, err := s.applySharesConfig(); err != nil {
		return fmt.Errorf("SMB account removed but pushing it to the node failed: %w", err)
	}
	return nil
}

// setSMBUserEnabled mirrors an LDAP enable/disable so a disabled user cannot
// keep authenticating over SMB.
func (s *Server) setSMBUserEnabled(uid string, enabled bool) error {
	if s.sambaUsers == nil {
		return nil
	}
	if err := s.sambaUsers.SetEnabled(uid, enabled); err != nil {
		return err
	}
	if _, err := s.applySharesConfig(); err != nil {
		return fmt.Errorf("SMB account state recorded but pushing it to the node failed: %w", err)
	}
	return nil
}
