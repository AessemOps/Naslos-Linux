package server

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	corev1 "k8s.io/api/core/v1"
	"k8s.io/client-go/tools/remotecommand"
)

// remotecommandSize is a small helper to keep the tests readable.
func remotecommandSize(cols, rows uint16) remotecommand.TerminalSize {
	return remotecommand.TerminalSize{Width: cols, Height: rows}
}

// TestSameHostname pins the origin check that a browser's websocket handshake
// depends on. nginx forwards `Host` without the UI's NodePort while the browser's
// Origin keeps it, so the comparison must ignore ports - a port-sensitive check
// rejected the terminal's own UI during development.
func TestSameHostname(t *testing.T) {
	ok := []struct{ a, b string }{
		{"192.168.1.96:30080", "192.168.1.96"},
		{"192.168.1.96", "192.168.1.96:30080"},
		{"192.168.1.96:30080", "192.168.1.96:30080"},
		{"NASLOS.local:443", "naslos.local"},
		{"[::1]:8080", "::1"},
	}
	for _, tc := range ok {
		if !sameHostname(tc.a, tc.b) {
			t.Errorf("sameHostname(%q, %q) = false, want true", tc.a, tc.b)
		}
	}

	// A different host must never be accepted, or any site could open a shell.
	bad := []struct{ a, b string }{
		{"evil.example:30080", "192.168.1.96"},
		{"192.168.1.97", "192.168.1.96:30080"},
		{"", "192.168.1.96"},
		{"evil-192.168.1.96", "192.168.1.96"},
	}
	for _, tc := range bad {
		if sameHostname(tc.a, tc.b) {
			t.Errorf("sameHostname(%q, %q) = true, want false", tc.a, tc.b)
		}
	}
}

// TestShellCommand keeps the endpoint from becoming "run anything as root": only
// shells are accepted, and TERM is always set (the exec API has no env field).
func TestShellCommand(t *testing.T) {
	for _, name := range ShellNames() {
		command, err := shellCommand(name)
		if err != nil {
			t.Fatalf("shellCommand(%q) = %v, want nil", name, err)
		}
		if command[0] != "/usr/bin/env" || command[1] != "TERM=xterm-256color" {
			t.Errorf("shellCommand(%q) = %v, want it to set TERM", name, command)
		}
	}

	// An empty shell defaults to sh, which exists in every image.
	if command, err := shellCommand(""); err != nil || command[len(command)-1] != "/bin/sh" {
		t.Errorf("shellCommand(\"\") = %v, %v, want /bin/sh", command, err)
	}
	// Case and whitespace should not matter.
	if _, err := shellCommand(" BASH "); err != nil {
		t.Errorf("shellCommand(\" BASH \") = %v, want nil", err)
	}

	for _, bad := range []string{"rm", "/bin/rm", "bash -c id", "sh;id", "python3"} {
		if _, err := shellCommand(bad); err == nil {
			t.Errorf("shellCommand(%q) = nil error, want a rejection", bad)
		}
	}
}

// TestResolveContainer covers the "which container?" decision. Guessing between
// several would attach the operator to the wrong process, so that case is an
// error that names the choices.
func TestResolveContainer(t *testing.T) {
	one := &corev1.Pod{Spec: corev1.PodSpec{Containers: []corev1.Container{{Name: "shell"}}}}
	if name, err := resolveContainer(one, ""); err != nil || name != "shell" {
		t.Errorf("resolveContainer(single) = %q, %v, want shell", name, err)
	}
	if name, err := resolveContainer(one, "shell"); err != nil || name != "shell" {
		t.Errorf("resolveContainer(requested) = %q, %v, want shell", name, err)
	}
	if _, err := resolveContainer(one, "api"); err == nil {
		t.Error("resolveContainer(unknown) = nil error, want a rejection")
	}

	several := &corev1.Pod{Spec: corev1.PodSpec{Containers: []corev1.Container{
		{Name: "api"}, {Name: "sidecar"},
	}}}
	if _, err := resolveContainer(several, ""); err == nil {
		t.Error("resolveContainer(several, empty) = nil error, want an error naming the containers")
	}
	if name, err := resolveContainer(several, "sidecar"); err != nil || name != "sidecar" {
		t.Errorf("resolveContainer(several, sidecar) = %q, %v", name, err)
	}
}

// TestRequireTerminalAuth pins the gate in front of a privileged shell: it must
// fail closed when the authenticated proxy's identity header is missing, and it
// must be possible to turn it off explicitly for development.
func TestRequireTerminalAuth(t *testing.T) {
	gated := &Server{terminalRequireAuth: true, terminalAuthHeader: "Remote-User"}

	// No header: refused, with a reason (the UI shows this text).
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/api/pods", nil)
	if gated.requireTerminalAuth(rec, req) {
		t.Error("requireTerminalAuth(no header) = true, want false")
	}
	if rec.Code != http.StatusUnauthorized {
		t.Errorf("status = %d, want 401", rec.Code)
	}
	if !strings.Contains(rec.Body.String(), "authenticated") {
		t.Errorf("body = %q, want it to explain that a session is required", rec.Body.String())
	}

	// Blank header (a client sending an empty value) must not pass either.
	rec = httptest.NewRecorder()
	req = httptest.NewRequest(http.MethodGet, "/api/pods", nil)
	req.Header.Set("Remote-User", "   ")
	if gated.requireTerminalAuth(rec, req) {
		t.Error("requireTerminalAuth(blank header) = true, want false")
	}

	// The header the authenticated proxy injects: allowed.
	rec = httptest.NewRecorder()
	req = httptest.NewRequest(http.MethodGet, "/api/pods", nil)
	req.Header.Set("Remote-User", "smbtest")
	if !gated.requireTerminalAuth(rec, req) {
		t.Error("requireTerminalAuth(with header) = false, want true")
	}
	if user := gated.terminalUsername(req); user != "smbtest" {
		t.Errorf("terminalUsername = %q, want smbtest", user)
	}

	// Development escape hatch.
	open := &Server{terminalRequireAuth: false, terminalAuthHeader: "Remote-User"}
	rec = httptest.NewRecorder()
	if !open.requireTerminalAuth(rec, httptest.NewRequest(http.MethodGet, "/api/pods", nil)) {
		t.Error("requireTerminalAuth(requireAuth=false) = false, want true")
	}

	// A differently named header is honoured (the chart makes it configurable).
	custom := &Server{terminalRequireAuth: true, terminalAuthHeader: "X-Auth-User"}
	rec = httptest.NewRecorder()
	req = httptest.NewRequest(http.MethodGet, "/api/pods", nil)
	req.Header.Set("Remote-User", "smbtest")
	if custom.requireTerminalAuth(rec, req) {
		t.Error("an unrelated header was accepted, want a refusal")
	}
}

// TestTerminalSizeQueue checks that a size pushed before the executor asks is
// still delivered (the UI sends one as soon as the socket opens) and that bursts
// keep the newest size instead of filling a queue.
func TestTerminalSizeQueue(t *testing.T) {
	q := newTerminalSizeQueue()

	q.push(remotecommandSize(80, 24))
	first := q.Next()
	if first == nil || first.Width != 80 || first.Height != 24 {
		t.Fatalf("Next() = %v, want 80x24", first)
	}

	// A burst: only the last size matters.
	q.push(remotecommandSize(100, 30))
	q.push(remotecommandSize(120, 40))
	next := q.Next()
	if next == nil || next.Width != 120 || next.Height != 40 {
		t.Errorf("Next() after a burst = %v, want 120x40", next)
	}
}

// TestValidateKubeName keeps caller-supplied names out of API paths.
func TestValidateKubeName(t *testing.T) {
	for _, ok := range []string{"naslos", "naslos-api-abc123", "kube-system", "ns.with.dots"} {
		if err := validateKubeName(ok, "pod"); err != nil {
			t.Errorf("validateKubeName(%q) = %v, want nil", ok, err)
		}
	}
	for _, bad := range []string{"", "  ", "Naslos", "ns/other", "../etc", "a b", "ns?x=1"} {
		if err := validateKubeName(bad, "pod"); err == nil {
			t.Errorf("validateKubeName(%q) = nil, want an error", bad)
		}
	}
}
