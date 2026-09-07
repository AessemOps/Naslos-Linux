package server

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
)

// handleUsers handles user collection operations.
func (s *Server) handleUsers(w http.ResponseWriter, r *http.Request) {
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

		writeJSON(w, http.StatusCreated, person)

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleUserDetail handles individual user operations.
func (s *Server) handleUserDetail(w http.ResponseWriter, r *http.Request) {
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
		var req struct {
			DisplayName string   `json:"displayName"`
			Email       string   `json:"email"`
			FirstName   string   `json:"firstName"`
			LastName    string   `json:"lastName"`
			Groups      []string `json:"groups"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		if err := s.identity.UpdatePerson(uid, req.DisplayName, req.Email, req.FirstName, req.LastName); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
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
