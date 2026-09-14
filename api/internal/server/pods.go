package server

import (
	"context"
	"fmt"
	"net/http"
	"sort"
	"strings"
	"sync"
	"time"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
)

// Kubernetes access for the terminal's pickers and exec sessions. The API speaks
// to the cluster with its own ServiceAccount; the chart grants it exactly pods
// get/list/watch, pods/log get and pods/exec create in its own namespace, plus
// namespaces get/list for the namespace picker (see templates/terminal.yaml).
var (
	k8sOnce   sync.Once
	k8sClient kubernetes.Interface
	k8sErr    error
)

// kubernetesClient returns a cached clientset: the API is one long-lived
// process, so a single client keeps the connection pool warm.
func (s *Server) kubernetesClient() (kubernetes.Interface, error) {
	k8sOnce.Do(func() {
		config, err := getKubeConfig(s.kubeconfig)
		if err != nil {
			k8sErr = err
			return
		}
		k8sClient, k8sErr = kubernetes.NewForConfig(config)
	})
	return k8sClient, k8sErr
}

// terminalPodName is the app.kubernetes.io/name label of the chart's privileged
// shell pod - the container the terminal is designed to attach to.
const terminalPodName = "naslos-terminal"

// podInfo is one row of the terminal's pod picker.
type podInfo struct {
	Name       string   `json:"name"`
	Namespace  string   `json:"namespace"`
	Containers []string `json:"containers"`
	Phase      string   `json:"phase"`
	Ready      bool     `json:"ready"`
	// Terminal marks the shell pod, so the UI can preselect it instead of
	// making the operator hunt for a pod name.
	Terminal bool `json:"terminal"`
}

// requireTerminalAuth gates the terminal endpoints (target discovery and exec) on
// evidence of an authenticated session, and fails closed.
//
// The evidence is the identity header the authenticated proxy injects -
// Authelia's Remote-User, forwarded by the Traefik forwardAuth middleware, which
// *replaces* any value a client sent. That replacement is what makes the header
// trustworthy; it also means the terminal must be reached through that proxy:
// the chart routes these paths from Traefik straight to the API, and the UI's
// nginx refuses them outright, so the unauthenticated NodePort cannot reach them
// even by sending the header itself.
//
// Set TERMINAL_REQUIRE_AUTH=false for local development (the API then allows
// anonymous terminal access and says so at startup).
func (s *Server) requireTerminalAuth(w http.ResponseWriter, r *http.Request) bool {
	if !s.terminalRequireAuth {
		return true
	}

	if user := strings.TrimSpace(r.Header.Get(s.terminalAuthHeader)); user != "" {
		return true
	}

	writeError(w, http.StatusUnauthorized,
		"the terminal requires an authenticated session. Reach the UI through an authenticating "+
			"proxy (the chart routes these paths from Traefik + Authelia straight to the API), or "+
			"set terminal.requireAuth=false to allow unauthenticated access on a trusted network")
	return false
}

// terminalUsername returns the authenticated user behind the request, for logs.
func (s *Server) terminalUsername(r *http.Request) string {
	if user := strings.TrimSpace(r.Header.Get(s.terminalAuthHeader)); user != "" {
		return user
	}
	return "anonymous"
}

// handleNamespaces lists namespaces, for the terminal's namespace picker.
func (s *Server) handleNamespaces(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if !s.requireTerminalAuth(w, r) {
		return
	}

	client, err := s.kubernetesClient()
	if err != nil {
		writeError(w, http.StatusServiceUnavailable, "cannot reach the Kubernetes API: "+err.Error())
		return
	}

	ctx, cancel := context.WithTimeout(r.Context(), 10*time.Second)
	defer cancel()

	list, err := client.CoreV1().Namespaces().List(ctx, metav1.ListOptions{})
	if err != nil {
		writeError(w, http.StatusBadGateway, "listing namespaces: "+err.Error())
		return
	}

	names := make([]string, 0, len(list.Items))
	for _, ns := range list.Items {
		names = append(names, ns.Name)
	}
	sort.Strings(names)
	writeJSON(w, http.StatusOK, names)
}

// handlePods lists pods with their containers, for the terminal's pickers.
// GET /api/pods?namespace=naslos (an empty namespace lists every namespace).
func (s *Server) handlePods(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if !s.requireTerminalAuth(w, r) {
		return
	}

	namespace := r.URL.Query().Get("namespace")
	if namespace != "" {
		if err := validateKubeName(namespace, "namespace"); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
	}

	client, err := s.kubernetesClient()
	if err != nil {
		writeError(w, http.StatusServiceUnavailable, "cannot reach the Kubernetes API: "+err.Error())
		return
	}

	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()

	list, err := client.CoreV1().Pods(namespace).List(ctx, metav1.ListOptions{})
	if err != nil {
		writeError(w, http.StatusBadGateway, "listing pods: "+err.Error())
		return
	}

	pods := make([]podInfo, 0, len(list.Items))
	for _, pod := range list.Items {
		pods = append(pods, describePod(pod))
	}
	sort.Slice(pods, func(i, j int) bool {
		if pods[i].Namespace != pods[j].Namespace {
			return pods[i].Namespace < pods[j].Namespace
		}
		return pods[i].Name < pods[j].Name
	})
	writeJSON(w, http.StatusOK, pods)
}

// describePod flattens a pod into what the pickers need.
func describePod(pod corev1.Pod) podInfo {
	info := podInfo{Name: pod.Name, Namespace: pod.Namespace, Phase: string(pod.Status.Phase)}
	for _, c := range pod.Spec.Containers {
		info.Containers = append(info.Containers, c.Name)
	}
	for _, cond := range pod.Status.Conditions {
		if cond.Type == corev1.PodReady && cond.Status == corev1.ConditionTrue {
			info.Ready = true
		}
	}
	info.Terminal = pod.Labels["app.kubernetes.io/name"] == terminalPodName
	return info
}

// resolveContainer decides which container to exec into: the requested one, or
// the pod's only container. With several candidates it is an error rather than a
// guess - attaching an operator to the wrong container is worse than asking.
func resolveContainer(pod *corev1.Pod, requested string) (string, error) {
	names := make([]string, 0, len(pod.Spec.Containers))
	for _, c := range pod.Spec.Containers {
		names = append(names, c.Name)
	}

	if requested != "" {
		for _, n := range names {
			if n == requested {
				return requested, nil
			}
		}
		return "", fmt.Errorf("pod %s has no container %q (containers: %s)",
			pod.Name, requested, strings.Join(names, ", "))
	}

	switch len(names) {
	case 0:
		return "", fmt.Errorf("pod %s has no containers", pod.Name)
	case 1:
		return names[0], nil
	default:
		return "", fmt.Errorf("pod %s has several containers, pick one: %s",
			pod.Name, strings.Join(names, ", "))
	}
}

// validateKubeName rejects anything that is not a DNS-1123 label/subdomain, so a
// caller-supplied name can never be interpolated into an API path.
func validateKubeName(name, what string) error {
	if strings.TrimSpace(name) == "" {
		return fmt.Errorf("%s is required", what)
	}
	if len(name) > 253 {
		return fmt.Errorf("%s is too long", what)
	}
	for _, r := range name {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9', r == '-', r == '.':
		default:
			return fmt.Errorf("invalid %s %q", what, name)
		}
	}
	return nil
}
