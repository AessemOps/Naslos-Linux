package server

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/gorilla/websocket"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes/scheme"
	"k8s.io/client-go/rest"
	"k8s.io/client-go/tools/clientcmd"
	"k8s.io/client-go/tools/remotecommand"
)

// upgrader accepts same-origin upgrades. The browser's Origin must match the host
// it is talking to; the comparison ignores ports, because the request arrives
// through the UI's nginx (which forwards `Host` without the port while the
// Origin keeps the NodePort) and the API itself only ever sees the proxy.
var upgrader = websocket.Upgrader{
	CheckOrigin: func(r *http.Request) bool {
		origin := r.Header.Get("Origin")
		if origin == "" {
			// Non-browser clients (the Playwright suite, CLI tools) send none.
			return true
		}

		parsed, err := url.Parse(origin)
		if err != nil {
			return false
		}

		hosts := []string{r.Host}
		if forwarded := r.Header.Get("X-Forwarded-Host"); forwarded != "" {
			hosts = append(hosts, forwarded)
		}
		for _, host := range hosts {
			if sameHostname(parsed.Hostname(), host) {
				return true
			}
		}
		return false
	},
	ReadBufferSize:  4096,
	WriteBufferSize: 4096,
}

// sameHostname compares two host[:port] values by hostname only.
func sameHostname(a, b string) bool {
	strip := func(value string) string {
		if host, _, err := net.SplitHostPort(value); err == nil {
			return strings.ToLower(host)
		}
		return strings.ToLower(strings.TrimSuffix(value, ":"))
	}
	return a != "" && strip(a) == strip(b)
}

// wsClient serialises writes to one websocket connection.
type wsClient struct {
	conn *websocket.Conn
	mu   sync.Mutex
}

func (c *wsClient) write(data []byte) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.conn.WriteMessage(websocket.TextMessage, data)
}

func (c *wsClient) close() error {
	return c.conn.Close()
}

// getKubeConfig returns the Kubernetes REST config. The API runs in-cluster, so
// an explicit kubeconfig is only used for local development.
func getKubeConfig(kubeconfig string) (*rest.Config, error) {
	if kubeconfig != "" {
		return clientcmd.BuildConfigFromFlags("", kubeconfig)
	}
	return rest.InClusterConfig()
}

// shells maps a shell name to the argv an exec session runs. Free-form commands
// are deliberately not accepted: this endpoint is reachable from the browser, so
// an allowlist keeps it from being "run any command as root". TERM is set here
// because the exec API has no environment field and a shell without TERM loses
// line editing and colour.
var shells = map[string][]string{
	"bash": {"/usr/bin/env", "TERM=xterm-256color", "/bin/bash", "-l"},
	"sh":   {"/usr/bin/env", "TERM=xterm-256color", "/bin/sh"},
	"ash":  {"/usr/bin/env", "TERM=xterm-256color", "/bin/ash"},
	"zsh":  {"/usr/bin/env", "TERM=xterm-256color", "/bin/zsh", "-l"},
}

// ShellNames lists the shells the terminal offers, in preference order.
func ShellNames() []string { return []string{"bash", "sh", "ash", "zsh"} }

// shellCommand resolves the requested shell, defaulting to sh (present in every
// image; bash is not).
func shellCommand(shell string) ([]string, error) {
	name := strings.ToLower(strings.TrimSpace(shell))
	if name == "" {
		name = "sh"
	}
	command, ok := shells[name]
	if !ok {
		return nil, fmt.Errorf("unsupported shell %q (supported: %s)", shell, strings.Join(ShellNames(), ", "))
	}
	return command, nil
}

// controlMessage is a text-frame message from the terminal: currently only a
// window resize.
type controlMessage struct {
	Type string `json:"type"`
	Cols uint16 `json:"cols"`
	Rows uint16 `json:"rows"`
}

// terminalSizeQueue feeds window sizes to the exec session. Next blocks until a
// size is available, which is what remotecommand expects; only the newest size
// matters, so pushes coalesce rather than queue up.
type terminalSizeQueue struct {
	mu      sync.Mutex
	current remotecommand.TerminalSize
	wake    chan struct{}
}

func newTerminalSizeQueue() *terminalSizeQueue {
	return &terminalSizeQueue{wake: make(chan struct{}, 1)}
}

// Next implements remotecommand.TerminalSizeQueue.
func (q *terminalSizeQueue) Next() *remotecommand.TerminalSize {
	<-q.wake
	q.mu.Lock()
	defer q.mu.Unlock()
	size := q.current
	return &size
}

func (q *terminalSizeQueue) push(size remotecommand.TerminalSize) {
	q.mu.Lock()
	q.current = size
	q.mu.Unlock()
	select {
	case q.wake <- struct{}{}:
	default:
		// A size is already waiting; the newer one replaced it.
	}
}

// wsInput pumps the websocket into the exec session's stdin.
//
// Frame type carries the meaning: binary frames are keystrokes, text frames are
// JSON control messages. Sending both as text would make the protocol ambiguous
// (a resize could be typed into the shell as garbage).
type wsInput struct {
	conn   *websocket.Conn
	sizes  *terminalSizeQueue
	reader *io.PipeReader
	writer *io.PipeWriter

	mu     sync.Mutex
	isDone bool
}

func newWSInput(conn *websocket.Conn) *wsInput {
	reader, writer := io.Pipe()
	return &wsInput{
		conn:   conn,
		sizes:  newTerminalSizeQueue(),
		reader: reader,
		writer: writer,
	}
}

// Read is the io.Reader the executor consumes stdin from.
func (i *wsInput) Read(p []byte) (int, error) { return i.reader.Read(p) }

// run reads frames until the socket closes, then unblocks Read so the exec
// stream can finish.
func (i *wsInput) run() {
	defer i.writer.Close()

	for {
		msgType, data, err := i.conn.ReadMessage()
		if err != nil {
			i.markClosed()
			return
		}

		if msgType == websocket.TextMessage {
			var msg controlMessage
			if json.Unmarshal(data, &msg) != nil {
				continue
			}
			if msg.Type == "resize" && msg.Cols > 0 && msg.Rows > 0 {
				i.sizes.push(remotecommand.TerminalSize{Width: msg.Cols, Height: msg.Rows})
			}
			continue
		}

		if _, err := i.writer.Write(data); err != nil {
			i.markClosed()
			return
		}
	}
}

func (i *wsInput) markClosed() {
	i.mu.Lock()
	i.isDone = true
	i.mu.Unlock()
}

// closed reports whether the socket went away, which is how a session normally
// ends (tab closed, Disconnect pressed) and must not be reported as an error.
func (i *wsInput) closed() bool {
	i.mu.Lock()
	defer i.mu.Unlock()
	return i.isDone
}

func (i *wsInput) close() { i.writer.Close() }

// handleLogsWS streams logs from a pod via WebSocket.
//
//	GET /api/ws/logs?namespace=&pod=&container=&tail=
func (s *Server) handleLogsWS(w http.ResponseWriter, r *http.Request) {
	namespace := r.URL.Query().Get("namespace")
	if namespace == "" {
		namespace = s.namespace
	}
	podName := r.URL.Query().Get("pod")
	container := r.URL.Query().Get("container")

	if podName == "" {
		writeError(w, http.StatusBadRequest, "pod parameter required")
		return
	}
	if err := validateKubeName(namespace, "namespace"); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	if err := validateKubeName(podName, "pod"); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	client, err := s.kubernetesClient()
	if err != nil {
		writeError(w, http.StatusServiceUnavailable, "cannot reach the Kubernetes API: "+err.Error())
		return
	}

	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		log.Printf("logs: websocket upgrade failed for %s/%s: %v", namespace, podName, err)
		return
	}
	ws := &wsClient{conn: conn}
	defer ws.close()

	tailLines := int64(200)
	logReq := client.CoreV1().Pods(namespace).GetLogs(podName, &corev1.PodLogOptions{
		Container: container,
		TailLines: &tailLines,
		Follow:    true,
	})

	stream, err := logReq.Stream(r.Context())
	if err != nil {
		ws.write([]byte("\r\n\x1b[31mCannot stream logs: " + err.Error() + "\x1b[0m\r\n"))
		return
	}
	defer stream.Close()

	buf := make([]byte, 4096)
	for {
		n, err := stream.Read(buf)
		if n > 0 {
			if writeErr := ws.write(buf[:n]); writeErr != nil {
				return
			}
		}
		if err == io.EOF {
			ws.write([]byte("\r\n\x1b[33m--- log stream ended ---\x1b[0m\r\n"))
			return
		}
		if err != nil {
			ws.write([]byte("\r\n\x1b[31mError reading logs: " + err.Error() + "\x1b[0m\r\n"))
			return
		}
	}
}

// handleExecWS opens an interactive exec session to a pod via WebSocket.
//
//	GET /api/ws/exec?namespace=&pod=&container=&shell=
//
// This is what the terminal page attaches to. Everything that can fail is
// resolved *before* the upgrade, so a bad pod/container/shell is an HTTP status
// the UI can show properly rather than text inside a terminal that just opened.
func (s *Server) handleExecWS(w http.ResponseWriter, r *http.Request) {
	// The terminal runs a root shell in a privileged container, so it is gated on
	// an authenticated session before anything else is even resolved.
	if !s.requireTerminalAuth(w, r) {
		return
	}

	namespace := r.URL.Query().Get("namespace")
	if namespace == "" {
		namespace = s.namespace
	}
	podName := r.URL.Query().Get("pod")
	container := r.URL.Query().Get("container")

	if podName == "" {
		writeError(w, http.StatusBadRequest, "pod parameter required")
		return
	}
	if err := validateKubeName(namespace, "namespace"); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	if err := validateKubeName(podName, "pod"); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	command, err := shellCommand(r.URL.Query().Get("shell"))
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	// A fresh client: the config is needed both for exec and for the SPDY
	// executor, which takes the whole REST config.
	config, err := getKubeConfig(s.kubeconfig)
	if err != nil {
		writeError(w, http.StatusServiceUnavailable, "cannot reach the Kubernetes API: "+err.Error())
		return
	}
	clientset, err := s.kubernetesClient()
	if err != nil {
		writeError(w, http.StatusServiceUnavailable, "cannot reach the Kubernetes API: "+err.Error())
		return
	}

	ctx, cancel := context.WithTimeout(r.Context(), 10*time.Second)
	defer cancel()

	pod, err := clientset.CoreV1().Pods(namespace).Get(ctx, podName, metav1.GetOptions{})
	if err != nil {
		writeError(w, http.StatusNotFound, "cannot find pod "+namespace+"/"+podName)
		return
	}
	container, err = resolveContainer(pod, container)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	if pod.Status.Phase != corev1.PodRunning {
		writeError(w, http.StatusConflict,
			fmt.Sprintf("pod %s is %s, not running", podName, pod.Status.Phase))
		return
	}

	// A plain GET (no Upgrade header) is a preflight: a browser cannot read the
	// HTTP status of a failed websocket handshake, so the terminal asks first to
	// get a real error message. Answering with the resolved target also documents
	// what the connection would use.
	if !websocket.IsWebSocketUpgrade(r) {
		shell := strings.ToLower(strings.TrimSpace(r.URL.Query().Get("shell")))
		if shell == "" {
			shell = "sh"
		}
		writeJSON(w, http.StatusOK, map[string]string{
			"namespace": namespace,
			"pod":       podName,
			"container": container,
			"shell":     shell,
			"status":    "ready to attach",
		})
		return
	}

	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		log.Printf("terminal: websocket upgrade failed for %s/%s: %v", namespace, podName, err)
		return
	}
	ws := &wsClient{conn: conn}
	defer ws.close()

	req := clientset.CoreV1().RESTClient().Post().
		Resource("pods").
		Name(podName).
		Namespace(namespace).
		SubResource("exec")

	req.VersionedParams(&corev1.PodExecOptions{
		Container: container,
		Command:   command,
		Stdin:     true,
		Stdout:    true,
		Stderr:    true,
		TTY:       true,
	}, scheme.ParameterCodec)

	executor, err := remotecommand.NewSPDYExecutor(config, "POST", req.URL())
	if err != nil {
		ws.write([]byte("\r\n\x1b[31mCannot create the exec session: " + err.Error() + "\x1b[0m\r\n"))
		return
	}

	stdin := newWSInput(conn)
	go stdin.run()
	defer stdin.close()

	log.Printf("terminal: session opened %s/%s (container %s, shell %s, user %s)",
		namespace, podName, container, strings.Join(command, " "), s.terminalUsername(r))

	err = executor.StreamWithContext(r.Context(), remotecommand.StreamOptions{
		Stdin:             stdin,
		Stdout:            &wsWriter{client: ws},
		Stderr:            &wsWriter{client: ws},
		Tty:               true,
		TerminalSizeQueue: stdin.sizes,
	})

	// A closed socket is the normal end of a session (tab closed, Disconnect
	// pressed); only a real failure is worth reporting in the terminal.
	if err != nil && !stdin.closed() {
		ws.write([]byte("\r\n\x1b[31mSession ended: " + err.Error() + "\x1b[0m\r\n"))
		log.Printf("terminal: session %s/%s ended: %v", namespace, podName, err)
	}
	log.Printf("terminal: session closed %s/%s", namespace, podName)
}

// wsWriter writes to a WebSocket client.
type wsWriter struct {
	client *wsClient
}

func (w *wsWriter) Write(p []byte) (int, error) {
	if err := w.client.write(p); err != nil {
		return 0, err
	}
	return len(p), nil
}
