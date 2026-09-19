package server

import (
	"encoding/json"
	"log"
	"net/http"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/identity"
)

// handleUsers handles user collection operations.
func (s *Server) handleUsers(w http.ResponseWriter, r *http.Request) {
	if s.identityUnavailable(w) {
		return
	}
	switch r.Method {
	case http.MethodGet:
		people, err := s.identity.ListPeople()
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, people)

	case http.MethodPost:
		var req struct {
			UID         string   `json:"uid"`
			DisplayName string   `json:"displayName"`
			Email       string   `json:"email"`
			FirstName   string   `json:"firstName"`
			LastName    string   `json:"lastName"`
			Password    string   `json:"password"`
			Groups      []string `json:"groups"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		if req.UID == "" || req.LastName == "" {
			writeError(w, http.StatusBadRequest, "uid and lastName are required")
			return
		}

		person, err := s.identity.CreatePerson(req.UID, req.DisplayName, req.Email, req.FirstName, req.LastName)
		if err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		if req.Password != "" {
			ntHash, err := s.identity.SetPassword(req.UID, req.Password)
			if err != nil {
				writeError(w, http.StatusInternalServerError, err.Error())
				return
			}
			if err := s.syncSMBPassword(req.UID, ntHash); err != nil {
				writeError(w, http.StatusInternalServerError, err.Error())
				return
			}
		}

		for _, group := range req.Groups {
			if err := s.identity.AddMember(group, req.UID); err != nil {
				writeError(w, http.StatusInternalServerError, err.Error())
				return
			}
		}

		// Group membership affects which shares the new user may use, so push
		// the refreshed mirror before answering.
		if err := s.refreshShareAccess(); err != nil {
			log.Printf("Warning: user created but pushing the share access mirror failed: %v", err)
		}

		writeJSON(w, http.StatusCreated, person)

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleUserDetail handles individual user operations.
func (s *Server) handleUserDetail(w http.ResponseWriter, r *http.Request) {
	if s.identityUnavailable(w) {
		return
	}
	uid := r.URL.Path[len("/api/users/"):]

	switch r.Method {
	case http.MethodGet:
		person, err := s.identity.GetPerson(uid)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, person)

	case http.MethodPut:
		// Groups is a pointer so an edit form that omits the field leaves
		// membership untouched, while an explicit list (including []) is applied
		// as a delta against the current membership.
		var req struct {
			DisplayName string    `json:"displayName"`
			Email       string    `json:"email"`
			FirstName   string    `json:"firstName"`
			LastName    string    `json:"lastName"`
			Groups      *[]string `json:"groups"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		if err := s.identity.UpdatePerson(uid, req.DisplayName, req.Email, req.FirstName, req.LastName); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		if req.Groups != nil {
			person, err := s.identity.GetPerson(uid)
			if err != nil {
				writeError(w, http.StatusNotFound, err.Error())
				return
			}
			// Validate with the same allowlist AddMember/RemoveMember enforce,
			// so an invalid name is a 400 instead of a comparison that silently
			// disagrees with the identity package.
			desired := make([]string, 0, len(*req.Groups))
			for _, group := range *req.Groups {
				normalized, err := identity.NormalizeGroupName(group)
				if err != nil {
					writeError(w, http.StatusBadRequest, err.Error())
					return
				}
				desired = append(desired, normalized)
			}

			add, remove := groupDelta(person.Groups, desired)
			if len(add) > 0 || len(remove) > 0 {
				membershipErr := applyMembershipChanges(add, remove,
					func(group string) error { return s.identity.AddMember(group, uid) },
					func(group string) error { return s.identity.RemoveMember(group, uid) },
				)
				// Push the mirror even after a partial failure, so the node's
				// view of membership matches what LDAP actually holds.
				if err := s.refreshShareAccess(); err != nil {
					log.Printf("Warning: user updated but pushing the share access mirror failed: %v", err)
				}
				if membershipErr != nil {
					writeError(w, http.StatusInternalServerError, membershipErr.Error())
					return
				}
			}
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "user updated"})

	case http.MethodDelete:
		if err := s.identity.DeletePerson(uid); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		s.removeSMBUser(uid)
		writeJSON(w, http.StatusOK, map[string]string{"status": "user deleted"})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// membershipDelta returns the entries to add and remove so that current becomes
// desired. normalize maps a raw entry (a short name, a uid or a DN) to its
// canonical comparison form; entries that normalize to nothing are ignored,
// duplicates collapse, adds keep the order of desired and removes the order of
// current. Both membership-editing endpoints share this one implementation.
func membershipDelta(current, desired []string, normalize func(string) string) (add, remove []string) {
	present := make(map[string]bool, len(current))
	for _, v := range current {
		if v = normalize(v); v != "" {
			present[v] = true
		}
	}
	wanted := make(map[string]bool, len(desired))
	for _, v := range desired {
		if v = normalize(v); v != "" {
			wanted[v] = true
		}
	}

	for _, v := range desired {
		v = normalize(v)
		if v == "" || present[v] {
			continue
		}
		present[v] = true // collapse duplicates within desired
		add = append(add, v)
	}
	for _, v := range current {
		v = normalize(v)
		if v == "" || wanted[v] {
			continue
		}
		wanted[v] = true // collapse duplicates within current
		remove = append(remove, v)
	}
	return add, remove
}

// groupDelta is membershipDelta for user-edit group names.
func groupDelta(current, desired []string) (add, remove []string) {
	return membershipDelta(current, desired, normalizeGroup)
}

// applyMembershipChanges applies every add then every remove, even when an
// earlier change failed, so one transient LDAP error cannot strand the rest of
// the delta. It returns the first error; the caller decides how to report it.
func applyMembershipChanges(add, remove []string, addOne, removeOne func(string) error) error {
	var firstErr error
	for _, v := range add {
		if err := addOne(v); err != nil && firstErr == nil {
			firstErr = err
		}
	}
	for _, v := range remove {
		if err := removeOne(v); err != nil && firstErr == nil {
			firstErr = err
		}
	}
	return firstErr
}

// normalizeGroup canonicalises an LDAP short group name with the identity
// package's rule (lowercase, trim, allowlist), so the comparison agrees with
// AddMember/RemoveMember. A value that fails validation is compared as its
// trimmed lowercase form rather than dropped; current values are LDAP-sourced
// and desired values were validated before the diff.
func normalizeGroup(g string) string {
	if normalized, err := identity.NormalizeGroupName(g); err == nil {
		return normalized
	}
	return strings.ToLower(strings.TrimSpace(g))
}
