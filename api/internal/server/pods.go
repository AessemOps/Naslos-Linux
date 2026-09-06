package server

import (
	"net/http"
)

// handlePodLogs streams logs from a pod.
func (s *Server) handlePodLogs(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	pod := r.URL.Query().Get("pod")
	namespace := r.URL.Query().Get("namespace")
	container := r.URL.Query().Get("container")

	if pod == "" {
		writeError(w, http.StatusBadRequest, "pod parameter required")
		return
	}

	// In production: use k8s.io/client-go kubernetes.CoreV1().Pods(ns).GetLogs(pod, opts).Stream()
	writeJSON(w, http.StatusOK, map[string]string{
		"status":    "streaming logs",
		"pod":       pod,
		"namespace": namespace,
		"container": container,
	})
}

// handlePodExec opens an exec session to a pod (WebSocket upgrade).
func (s *Server) handlePodExec(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	pod := r.URL.Query().Get("pod")
	namespace := r.URL.Query().Get("namespace")
	container := r.URL.Query().Get("container")
	command := r.URL.Query().Get("command")

	if pod == "" {
		writeError(w, http.StatusBadRequest, "pod parameter required")
		return
	}

	// In production: upgrade to WebSocket, use SPDY executor
	writeJSON(w, http.StatusOK, map[string]string{
		"status":    "exec session",
		"pod":       pod,
		"namespace": namespace,
		"container": container,
		"command":   command,
	})
}
