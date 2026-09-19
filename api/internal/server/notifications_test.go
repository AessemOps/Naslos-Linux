package server

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/AessemOps/Naslos-Linux/api/internal/notifications"
)

// doNotifications issues a request straight at the composed router. The server
// is built with AUTH_DISABLED so the test targets the handler, not the gate
// (the gate is covered by routes_test.go).
func doNotifications(t *testing.T, s *Server, method, body string) *httptest.ResponseRecorder {
	t.Helper()
	req := httptest.NewRequest(method, "/api/notifications", strings.NewReader(body))
	rec := httptest.NewRecorder()
	s.router.ServeHTTP(rec, req)
	return rec
}

// TestNotificationsNeverReturnTheAuthToken is the CR-07 regression test: the
// browser sees only `hasAuthToken`, never the credential, and an ordinary save
// that omits the field does not wipe the stored token.
func TestNotificationsNeverReturnTheAuthToken(t *testing.T) {
	s := newTestServer(t, true)

	token := "tk_super_secret"
	if err := s.notifications.UpdateSettings(notifications.Settings{
		Enabled:   true,
		ServerURL: "https://ntfy.example",
		Topic:     "naslos-prod",
	}, &token); err != nil {
		t.Fatalf("seeding the token: %v", err)
	}

	// GET: reports presence, never the value.
	rec := doNotifications(t, s, http.MethodGet, "")
	if rec.Code != http.StatusOK {
		t.Fatalf("GET /api/notifications: status = %d, want 200", rec.Code)
	}
	body := rec.Body.String()
	if !strings.Contains(body, `"hasAuthToken":true`) {
		t.Errorf("GET body = %s, want hasAuthToken:true", body)
	}
	if strings.Contains(body, token) {
		t.Errorf("GET body leaks the auth token: %s", body)
	}
	if strings.Contains(body, `"authToken"`) {
		t.Errorf("GET body still carries an authToken field: %s", body)
	}

	// PUT that omits authToken keeps the stored token.
	rec = doNotifications(t, s, http.MethodPut,
		`{"enabled":true,"serverUrl":"https://ntfy.example","topic":"renamed","email":"","enabledEvents":[],"minSeverity":"warning"}`)
	if rec.Code != http.StatusOK {
		t.Fatalf("PUT without a token: status = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	if got := s.notifications.GetSettings(); got.AuthToken != token || got.Topic != "renamed" {
		t.Errorf("after a token-less PUT: %+v, want topic renamed and the stored token", got)
	}

	// PUT with a value replaces it.
	rec = doNotifications(t, s, http.MethodPut,
		`{"enabled":true,"serverUrl":"https://ntfy.example","topic":"renamed","authToken":"tk_new","email":"","enabledEvents":[],"minSeverity":"warning"}`)
	if rec.Code != http.StatusOK {
		t.Fatalf("PUT replacing the token: status = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	if got := s.notifications.GetSettings(); got.AuthToken != "tk_new" {
		t.Errorf("after a replacing PUT: token = %q, want tk_new", got.AuthToken)
	}

	// PUT with an explicit empty string clears it, and GET now reports absence.
	rec = doNotifications(t, s, http.MethodPut,
		`{"enabled":true,"serverUrl":"https://ntfy.example","topic":"renamed","authToken":"","email":"","enabledEvents":[],"minSeverity":"warning"}`)
	if rec.Code != http.StatusOK {
		t.Fatalf("PUT clearing the token: status = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	if got := s.notifications.GetSettings(); got.AuthToken != "" {
		t.Errorf("after a clearing PUT: token = %q, want empty", got.AuthToken)
	}
	rec = doNotifications(t, s, http.MethodGet, "")
	if body := rec.Body.String(); !strings.Contains(body, `"hasAuthToken":false`) {
		t.Errorf("GET after clearing = %s, want hasAuthToken:false", body)
	}
}
