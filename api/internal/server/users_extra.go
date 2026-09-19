package server

import (
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/auth"
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

		// Both sides are compared as uids. GET returns member DNs while
		// Add/RemoveMember take uids, so normalising here means a client can
		// safely PUT back exactly what it read - otherwise a DN would be
		// treated as a uid and stored as a malformed member
		// ("uid=uid=alice,ou=people,...").
		add, remove := membershipDelta(group.Members, req.Members, extractUID)
		if err := applyMembershipChanges(add, remove,
			func(uid string) error { return s.identity.AddMember(cn, uid) },
			func(uid string) error { return s.identity.RemoveMember(cn, uid) },
		); err != nil {
			log.Printf("Warning: group %s updated with errors: %v", cn, err)
		}
		// Share access is evaluated by Samba against the group membership
		// mirrored on the node, so a membership change must re-push it -
		// otherwise a newly added member keeps being refused (and a removed one
		// keeps being allowed) until something else triggers an apply.
		if err := s.refreshShareAccess(); err != nil {
			log.Printf("Warning: group membership changed but pushing it to the node failed: %v", err)
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "group updated"})

	case http.MethodDelete:
		if err := s.identity.DeleteGroup(cn); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		// A deleted group must disappear from the node's mirror, or shares
		// restricted to it would keep granting access to its last members.
		if err := s.refreshShareAccess(); err != nil {
			log.Printf("Warning: group deleted but pushing it to the node failed: %v", err)
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "group deleted"})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleAuthMe returns the current authenticated user. The identity comes from
// the auth middleware's context (populated only for a request that carried the
// proxy secret), never from the raw header.
func (s *Server) handleAuthMe(w http.ResponseWriter, r *http.Request) {
	if s.identityUnavailable(w) {
		return
	}
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	user := auth.UserFromContext(r.Context())
	if user == nil || strings.TrimSpace(user.Username) == "" {
		writeError(w, http.StatusUnauthorized, "not authenticated")
		return
	}

	writeJSON(w, http.StatusOK, map[string]interface{}{
		"username":    user.Username,
		"groups":      strings.Join(user.Groups, ","),
		"email":       user.Email,
		"displayName": user.DisplayName,
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

// extractUID reduces a member reference to a bare uid. It accepts both a
// person DN ("uid=alice,ou=people,dc=naslos,dc=local") and a plain uid, because
// GET returns DNs and PUT is documented to take uids.
func extractUID(dn string) string {
	dn = strings.TrimSpace(dn)
	if dn == "" {
		return ""
	}
	if !strings.Contains(dn, "=") {
		return dn
	}

	// Take the value of the first uid= RDN, unescaping the DN value.
	value := strings.TrimPrefix(strings.Split(dn, ",")[0], "uid=")
	value = strings.NewReplacer(`\2C`, ",", `\3D`, "=", `\2c`, ",", `\3d`, "=").Replace(value)

	// Defend against a member that was itself stored as a DN (a historical
	// malformed entry looks like "uid=uid=alice"): keep the innermost value.
	if idx := strings.LastIndex(value, "uid="); idx >= 0 {
		value = value[idx+len("uid="):]
	}
	return strings.TrimSpace(value)
}

// Store accessor for the SMB account mirror. The store is created alongside
// the share manager; tests may construct a Server without one.
func (s *Server) smbUsers() *shares.SambaUserStore {
	return s.sambaUsers
}

// refreshShareAccess re-renders and pushes the share configuration plus the
// account/group mirrors. Called after anything that changes the identity data
// the node evaluates access against (group membership, group delete), because
// Samba reads that mirror rather than asking LDAP.
func (s *Server) refreshShareAccess() error {
	_, err := s.applySharesConfig()
	return err
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

	uidNumber, gidNumber := 0, 0
	if s.identity != nil {
		u, g, err := s.identity.GetPosixIDs(uid)
		if err == nil {
			uidNumber, gidNumber = u, g
		} else {
			log.Printf("Warning: could not read POSIX ids for %s (SMB login will need a local account): %v", uid, err)
		}
	}

	if err := s.sambaUsers.Upsert(shares.PosixIdentity{
		UID:    uid,
		UIDNum: uidNumber,
		GIDNum: gidNumber,
		Gecos:  uid,
	}, ntHash); err != nil {
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
