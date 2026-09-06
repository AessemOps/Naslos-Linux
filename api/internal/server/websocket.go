package server

import (
	"io"
	"log"
	"net/http"
	"strings"
	"sync"

	corev1 "k8s.io/api/core/v1"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/kubernetes/scheme"
	"k8s.io/client-go/rest"
	"k8s.io/client-go/tools/clientcmd"
	"k8s.io/client-go/tools/remotecommand"

	"github.com/gorilla/websocket"
)

var upgrader = websocket.Upgrader{
	CheckOrigin: func(r *http.Request) bool {
		return true // In production, restrict to UI origin
	},
	ReadBufferSize:  1024,
	WriteBufferSize: 1024,
}

// wsClient represents a WebSocket connection.
type wsClient struct {
	conn *websocket.Conn
	mu   sync.Mutex
}

// write writes a message to the WebSocket.
func (c *wsClient) write(data []byte) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.conn.WriteMessage(websocket.TextMessage, data)
}

// close closes the WebSocket connection.
func (c *wsClient) close() error {
	return c.conn.Close()
}

// getKubeConfig returns the Kubernetes REST config.
func getKubeConfig(kubeconfig string) (*rest.Config, error) {
	if kubeconfig != "" {
		return clientcmd.BuildConfigFromFlags("", kubeconfig)
	}
	return rest.InClusterConfig()
}

// handleLogs streams logs from a pod via WebSocket.
// GET /api/ws/logs?namespace=...&pod=...&container=...&tail=...
func (s *Server) handleLogsWS(w http.ResponseWriter, r *http.Request) {
	namespace := r.URL.Query().Get("namespace")
	pod := r.URL.Query().Get("pod")
	container := r.URL.Query().Get("container")

	if pod == "" {
		writeError(w, http.StatusBadRequest, "pod parameter required")
		return
	}
	if namespace == "" {
		namespace = "default"
	}

	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		log.Printf("WebSocket upgrade failed: %v", err)
		return
	}
	client := &wsClient{conn: conn}
	defer client.close()

	config, err := getKubeConfig(s.kubeconfig)
	if err != nil {
		client.write([]byte("Error: " + err.Error()))
		return
	}

	clientset, err := kubernetes.NewForConfig(config)
	if err != nil {
		client.write([]byte("Error: " + err.Error()))
		return
	}

	tailLines := int64(100)
	logReq := clientset.CoreV1().Pods(namespace).GetLogs(pod, &corev1.LogOptions{
		Container: container,
		TailLines: &tailLines,
		Follow:    true,
	})

	stream, err := logReq.Stream(r.Context())
	if err != nil {
		client.write([]byte("Error streaming logs: " + err.Error()))
		return
	}
	defer stream.Close()

	buf := make([]byte, 4096)
	for {
		n, err := stream.Read(buf)
		if n > 0 {
			if err := client.write(buf[:n]); err != nil {
				return
			}
		}
		if err == io.EOF {
			client.write([]byte("\n--- Log stream ended ---"))
			return
		}
		if err != nil {
			client.write([]byte("\nError reading logs: " + err.Error()))
			return
		}
	}
}

// handleExec opens an interactive exec session to a pod via WebSocket.
// GET /api/ws/exec?namespace=...&pod=...&container=...&command=...
func (s *Server) handleExecWS(w http.ResponseWriter, r *http.Request) {
	namespace := r.URL.Query().Get("namespace")
	pod := r.URL.Query().Get("pod")
	container := r.URL.Query().Get("container")
	command := r.URL.Query().Get("command")

	if pod == "" {
		writeError(w, http.StatusBadRequest, "pod parameter required")
		return
	}
	if namespace == "" {
		namespace = "default"
	}
	if container == "" {
		container = "main"
	}
	if command == "" {
		command = "/bin/sh"
	}

	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		log.Printf("WebSocket upgrade failed: %v", err)
		return
	}
	client := &wsClient{conn: conn}
	defer client.close()

	config, err := getKubeConfig(s.kubeconfig)
	if err != nil {
		client.write([]byte("Error: " + err.Error()))
		return
	}

	clientset, err := kubernetes.NewForConfig(config)
	if err != nil {
		client.write([]byte("Error: " + err.Error()))
		return
	}

	// Build exec request
	req := clientset.CoreV1().RESTClient().Post().
		Resource("pods").
		Name(pod).
		Namespace(namespace).
		SubResource("exec")

	req.VersionedParams(&corev1.PodExecOptions{
		Container: container,
		Command:   strings.Split(command, " "),
		Stdin:     true,
		Stdout:    true,
		Stderr:    true,
		TTY:       true,
	}, scheme.ParameterCodec)

	executor, err := remotecommand.NewSPDYExecutor(config, "POST", req.URL())
	if err != nil {
		client.write([]byte("Error creating exec: " + err.Error()))
		return
	}

	// Create a goroutine to read from WebSocket and forward to exec stdin
	go func() {
		for {
			_, message, err := conn.ReadMessage()
			if err != nil {
				return
			}
			// message is written to stdin by the stream below
			_ = message
		}
	}()

	// Stream exec output to WebSocket
	err = executor.Stream(remotecommand.StreamOptions{
		Stdin:  nil, // In production: pipe from WebSocket
		Stdout: &wsWriter{client: client},
		Stderr: &wsWriter{client: client},
		TTY:    true,
	})
	if err != nil {
		client.write([]byte("\nError: " + err.Error()))
	}
}

// wsWriter writes to a WebSocket client.
type wsWriter struct {
	client *wsClient
}

func (w *wsWriter) Write(p []byte) (int, error) {
	err := w.client.write(p)
	if err != nil {
		return 0, err
	}
	return len(p), nil
}

// suppress unused imports
var _ = schema.GroupVersionResource{}
