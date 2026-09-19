package agent

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
)

// TestEveryRequestCarriesTheAgentToken pins NAS-002 for the streaming paths. The
// timeout-free client behind SendStream/ReceiveStream is a separate
// *http.Client, and it once used the default transport: every `zfs send` and
// receive then went out without an Authorization header and the agent answered
// 401 ("missing or invalid agent token") as soon as auth was enabled. Both
// clients must carry the token.
func TestEveryRequestCarriesTheAgentToken(t *testing.T) {
	var mu sync.Mutex
	var headers []string

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		headers = append(headers, r.Header.Get("Authorization"))
		mu.Unlock()

		if r.URL.Query().Get("estimate") == "true" {
			_ = json.NewEncoder(w).Encode(map[string]int64{"bytes": 1})
			return
		}
		_, _ = w.Write([]byte("ok"))
	}))
	defer srv.Close()

	c := NewClient(srv.URL, "tok_test")

	// Control-plane call, through c.http.
	if _, err := c.EstimateSend(context.Background(), SendStreamOptions{Dataset: "pool/ds", To: "snap"}); err != nil {
		t.Fatalf("EstimateSend: %v", err)
	}

	// Streaming call, through c.stream.
	body, err := c.SendStream(context.Background(), SendStreamOptions{Dataset: "pool/ds", To: "snap"})
	if err != nil {
		t.Fatalf("SendStream: %v", err)
	}
	_, _ = io.Copy(io.Discard, body)
	_ = body.Close()

	mu.Lock()
	defer mu.Unlock()

	if len(headers) != 2 {
		t.Fatalf("expected both calls to reach the agent, got %d", len(headers))
	}
	for i, got := range headers {
		if !strings.HasPrefix(got, "Bearer ") || got != "Bearer tok_test" {
			t.Errorf("request %d Authorization = %q, want %q", i, got, "Bearer tok_test")
		}
	}
}
