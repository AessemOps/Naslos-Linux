package server

import (
	"bytes"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/agent"
	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
)

// The instance-side sender is verified against a *fake agent* (which streams a
// canned `zfs send` and consumes a `zfs receive`) and a *real buddy receiver*, so
// the tests cover the whole chain - snapshot, stream, encrypt, upload, manifest,
// restore, receive - without a pool.

// fakeAgent answers the agent's streaming endpoints.
type fakeAgent struct {
	server *httptest.Server

	mu          sync.Mutex
	snapshots   []agent.SnapshotInfo
	snapshotSeq int
	payload     []byte
	// estimateOverride, when non-zero, is what the dry run reports (default: the
	// payload length).
	estimateOverride int64
	// truncateTo, when non-zero, makes the send deliver only that many bytes while
	// the dry run still advertises the whole payload: a `zfs send` that died in the
	// middle.
	truncateTo   int
	sendCalls    []string
	receiveCalls []string
	received     bytes.Buffer
	// receiveErr makes the faked `zfs receive` refuse the stream.
	receiveErr string
	// sendDelay, when set, stalls the send stream (a slow `zfs send`), so a
	// test can hold a job in running state and exercise the 409 path.
	sendDelay time.Duration
}

func newFakeAgent(t *testing.T, payload []byte) *fakeAgent {
	t.Helper()

	fake := &fakeAgent{payload: payload}
	mux := http.NewServeMux()

	// Snapshot creation (POST /api/v1/snapshots/{dataset}).
	mux.HandleFunc("/api/v1/snapshots/", func(w http.ResponseWriter, r *http.Request) {
		var request struct {
			Name string `json:"name"`
		}
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			http.Error(w, fmt.Sprintf(`{"error":%q}`, err.Error()), http.StatusBadRequest)
			return
		}

		fake.mu.Lock()
		fake.snapshotSeq++
		fake.snapshots = append(fake.snapshots, agent.SnapshotInfo{
			Name:    request.Name,
			GUID:    strconv.Itoa(1000 + fake.snapshotSeq),
			Created: strconv.FormatInt(time.Now().Unix(), 10),
		})
		fake.mu.Unlock()

		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"status":"snapshot created"}`))
	})

	// Snapshot listing with GUIDs (GET /api/v1/zfs/snapshots/{dataset}).
	mux.HandleFunc("/api/v1/zfs/snapshots/", func(w http.ResponseWriter, _ *http.Request) {
		fake.mu.Lock()
		snapshots := append([]agent.SnapshotInfo(nil), fake.snapshots...)
		fake.mu.Unlock()

		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(snapshots)
	})

	// Send and estimate (GET /api/v1/zfs/send/{dataset}).
	mux.HandleFunc("/api/v1/zfs/send/", func(w http.ResponseWriter, r *http.Request) {
		query := r.URL.Query()
		fake.mu.Lock()
		fake.sendCalls = append(fake.sendCalls, fmt.Sprintf("%s to=%s from=%s raw=%s",
			strings.TrimPrefix(r.URL.Path, "/api/v1/zfs/send/"), query.Get("to"), query.Get("from"), query.Get("raw")))
		payload := fake.payload
		estimate := fake.estimateOverride
		truncateTo := fake.truncateTo
		delay := fake.sendDelay
		fake.mu.Unlock()

		if query.Get("estimate") == "true" {
			if estimate == 0 {
				estimate = int64(len(payload))
			}
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(map[string]any{"bytes": estimate})
			return
		}

		if truncateTo > 0 && truncateTo < len(payload) {
			payload = payload[:truncateTo]
		}
		if delay > 0 {
			time.Sleep(delay)
		}
		w.Header().Set("Content-Type", "application/octet-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write(payload)
	})

	// Receive (POST /api/v1/zfs/receive/{dataset}).
	mux.HandleFunc("/api/v1/zfs/receive/", func(w http.ResponseWriter, r *http.Request) {
		fake.mu.Lock()
		fake.receiveCalls = append(fake.receiveCalls, fmt.Sprintf("%s force=%s",
			strings.TrimPrefix(r.URL.Path, "/api/v1/zfs/receive/"), r.URL.Query().Get("force")))
		receiveErr := fake.receiveErr
		fake.mu.Unlock()

		body, _ := io.ReadAll(r.Body)
		fake.mu.Lock()
		fake.received.Write(body)
		fake.mu.Unlock()

		w.Header().Set("Content-Type", "application/json")
		if receiveErr != "" {
			w.WriteHeader(http.StatusUnprocessableEntity)
			_, _ = fmt.Fprintf(w, `{"error":%q}`, receiveErr)
			return
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"status": "received", "bytes": len(body)})
	})

	fake.server = httptest.NewServer(mux)
	t.Cleanup(fake.server.Close)
	return fake
}

// senderHarness is a Server wired to a fake agent, a real receiver and an identity.
type senderHarness struct {
	server      *Server
	agent       *fakeAgent
	receiver    *buddy.Receiver
	identity    *buddy.Identity
	receiverURL string
}

// newSenderHarness builds the harness. The identity file lives in a temp dir, so
// BUDDY_IDENTITY is set for the duration of the test.
func newSenderHarness(t *testing.T, payload []byte) *senderHarness {
	t.Helper()

	identity, err := buddy.NewIdentity("naslos-test")
	if err != nil {
		t.Fatalf("creating identity: %v", err)
	}
	identityPath := filepath.Join(t.TempDir(), "identity.json")
	if err := identity.Save(identityPath); err != nil {
		t.Fatalf("saving identity: %v", err)
	}
	t.Setenv("BUDDY_IDENTITY", identityPath)

	peers := buddy.NewPeerStore(filepath.Join(t.TempDir(), "peers.json"))
	if err := peers.Load(); err != nil {
		t.Fatalf("loading peers: %v", err)
	}
	receiver := &buddy.Receiver{
		Store:   buddy.NewStore(filepath.Join(t.TempDir(), "data")),
		Peers:   peers,
		Auth:    buddy.NewAuthenticator(peers),
		Name:    "fake-buddy",
		Version: "test",
	}
	// Loopback: the instance sends to itself, so its own key is authorized on its
	// own receiver.
	if err := peers.Add(&buddy.Peer{Name: identity.Name, PublicKey: identity.PublicKey, Enabled: true}); err != nil {
		t.Fatalf("authorizing the identity: %v", err)
	}
	receiverServer := httptest.NewServer(receiver.Handler())
	t.Cleanup(receiverServer.Close)

	fake := newFakeAgent(t, payload)
	// authDisabled is the same development opt-out the operator can set; the
	// owner auth gate itself is covered by routes_test.go.
	server := &Server{
		agent:        agent.NewClient(fake.server.URL, ""),
		buddy:        receiver,
		authDisabled: true,
		router:       http.NewServeMux(),
	}
	server.routes()

	return &senderHarness{
		server:      server,
		agent:       fake,
		receiver:    receiver,
		identity:    identity,
		receiverURL: receiverServer.URL,
	}
}

// call performs a request against the harness (the auth gate is off in these
// tests, so the handler sees it directly).
func (h *senderHarness) call(t *testing.T, method, path string, body any) *httptest.ResponseRecorder {
	t.Helper()

	var reader io.Reader
	if body != nil {
		encoded, err := json.Marshal(body)
		if err != nil {
			t.Fatalf("encoding request: %v", err)
		}
		reader = bytes.NewReader(encoded)
	}
	req := httptest.NewRequest(method, path, reader)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}

	rec := httptest.NewRecorder()
	h.server.router.ServeHTTP(rec, req)
	return rec
}

// decode unmarshals a response body, failing the test on error.
func decode(t *testing.T, rec *httptest.ResponseRecorder, out any) {
	t.Helper()

	if err := json.Unmarshal(rec.Body.Bytes(), out); err != nil {
		t.Fatalf("decoding response %q: %v", rec.Body.String(), err)
	}
}

// startSend POSTs /api/buddy/send and expects the async 202 with a job id.
func startSend(t *testing.T, h *senderHarness, body map[string]any) string {
	t.Helper()

	rec := h.call(t, http.MethodPost, "/api/buddy/send", body)
	if rec.Code != http.StatusAccepted {
		t.Fatalf("send = %d, want 202 (%s)", rec.Code, rec.Body.String())
	}
	var started struct {
		JobID  string `json:"jobId"`
		Status string `json:"status"`
	}
	decode(t, rec, &started)
	if started.JobID == "" || started.Status != "started" {
		t.Fatalf("start response = %s, want a jobId and started status", rec.Body.String())
	}
	return started.JobID
}

// waitSend waits for a job to finish and returns its snapshot.
func waitSend(t *testing.T, h *senderHarness, jobID string) buddyJobPublic {
	t.Helper()

	job := h.server.waitBuddyJob(jobID, 60*time.Second)
	if job == nil {
		t.Fatalf("job %s did not finish in time", jobID)
	}
	return *job
}

// sendAndWait starts a send and waits for it to succeed, returning the result.
func sendAndWait(t *testing.T, h *senderHarness, dataset, source string) buddySendResult {
	t.Helper()

	jobID := startSend(t, h, map[string]any{
		"dataset":  dataset,
		"source":   source,
		"receiver": h.receiverURL,
	})
	job := waitSend(t, h, jobID)
	if job.State != buddyJobSucceeded || job.Result == nil {
		t.Fatalf("job %s = %s (%s), want succeeded", jobID, job.State, job.Error)
	}
	return *job.Result
}

func TestBuddyIdentityLifecycle(t *testing.T) {
	harness := newSenderHarness(t, []byte("payload"))

	// A fresh instance has no identity, and the answer says what to do about it.
	rec := harness.call(t, http.MethodGet, "/api/buddy/identity", nil)
	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", rec.Code)
	}

	// The harness created one already, so replace it with a known name and check
	// that creating again without "replace" is refused: replacing an identity
	// orphans every backup this instance ever made.
	rec = harness.call(t, http.MethodPost, "/api/buddy/identity", map[string]any{"name": "naslos-a"})
	if rec.Code != http.StatusConflict {
		t.Fatalf("second create = %d, want 409 (%s)", rec.Code, rec.Body.String())
	}

	rec = harness.call(t, http.MethodPost, "/api/buddy/identity", map[string]any{"name": "naslos-a", "replace": true})
	if rec.Code != http.StatusCreated {
		t.Fatalf("replace = %d, want 201 (%s)", rec.Code, rec.Body.String())
	}
	var created struct {
		PublicKey   string `json:"publicKey"`
		Fingerprint string `json:"fingerprint"`
		Warning     string `json:"warning"`
	}
	decode(t, rec, &created)
	if !strings.HasPrefix(created.PublicKey, "ssh-ed25519 ") {
		t.Errorf("public key = %q, want an ssh-ed25519 line", created.PublicKey)
	}
	if !strings.HasPrefix(created.Fingerprint, "SHA256:") {
		t.Errorf("fingerprint = %q, want SSH256:…", created.Fingerprint)
	}
	if created.Warning == "" {
		t.Error("the response did not warn that the identity file must be backed up")
	}

	rec = harness.call(t, http.MethodGet, "/api/buddy/identity", nil)
	var fetched struct {
		Exists    bool   `json:"exists"`
		PublicKey string `json:"publicKey"`
	}
	decode(t, rec, &fetched)
	if !fetched.Exists || fetched.PublicKey != created.PublicKey {
		t.Errorf("GET identity = %+v, want the key just created", fetched)
	}
	if strings.Contains(rec.Body.String(), "privateKey") || strings.Contains(rec.Body.String(), "kek") {
		t.Error("the identity response leaked private material")
	}
}

// The owner gate on /api/buddy/* (identity, send, restore, jobs, schedules,
// peers, status) is asserted exhaustively in routes_test.go: every owner path
// must answer 401 without the proxy secret. These handler tests run with the
// same AUTH_DISABLED opt-out the operator can set, so they exercise the handlers
// themselves rather than the gate.

func TestBuddySendFullThenIncremental(t *testing.T) {
	// Not a whole number of chunks: the tail is the case a naive sender gets wrong.
	payload := bytes.Repeat([]byte("zfs-send-stream-payload-"), (3<<20)/24+1)
	harness := newSenderHarness(t, payload)
	source := "naslos-test/test"

	first := sendAndWait(t, harness, "test/data", source)

	if first.Status != "backed up" || first.Incremental || first.Base != "" {
		t.Errorf("first send = %+v, want a full backup", first)
	}
	if !strings.HasPrefix(first.Snapshot, "buddy-") {
		t.Errorf("snapshot = %q, want a buddy-… name", first.Snapshot)
	}
	if first.PlainBytes != int64(len(payload)) {
		t.Errorf("plain bytes = %d, want %d", first.PlainBytes, len(payload))
	}
	if first.Chunks < 3 {
		t.Errorf("chunks = %d, want the payload split into at least 3", first.Chunks)
	}
	if first.ToGUID == "" {
		t.Error("the send did not record the snapshot's GUID, so the next one cannot be incremental")
	}

	// What the buddy stored must decrypt back to exactly the stream the agent
	// produced: this is the whole point of the exercise.
	client := buddy.NewClient(harness.receiverURL, harness.identity)
	var restored bytes.Buffer
	result, err := client.Restore(buddy.RestoreOptions{Source: source, Out: &restored})
	if err != nil {
		t.Fatalf("restoring what was sent: %v", err)
	}
	if !bytes.Equal(restored.Bytes(), payload) {
		t.Error("the restored stream differs from what the agent sent")
	}
	if result.Manifest.ToSnapshot != first.Snapshot || result.Manifest.ToGUID != first.ToGUID {
		t.Errorf("manifest = snap %q guid %q, want %q/%q",
			result.Manifest.ToSnapshot, result.Manifest.ToGUID, first.Snapshot, first.ToGUID)
	}
	if result.Manifest.Kind != "zfs-send" {
		t.Errorf("kind = %q, want zfs-send", result.Manifest.Kind)
	}

	// Second send: the buddy already holds the first snapshot's GUID, so the agent
	// must be asked for an incremental stream.
	second := sendAndWait(t, harness, "test/data", source)

	if !second.Incremental || second.Base != first.Snapshot {
		t.Errorf("second send = %+v, want an incremental from %q", second, first.Snapshot)
	}
	if second.Chain == first.Chain {
		t.Error("the second send reused the first chain: each push is its own chain")
	}

	harness.agent.mu.Lock()
	calls := append([]string(nil), harness.agent.sendCalls...)
	harness.agent.mu.Unlock()
	if len(calls) < 3 {
		t.Fatalf("agent saw %d send calls, want an estimate and a send per push: %v", len(calls), calls)
	}
	last := calls[len(calls)-1]
	if !strings.Contains(last, "from="+first.Snapshot) {
		t.Errorf("last send call = %q, want it to carry from=%s", last, first.Snapshot)
	}
	if !strings.Contains(last, "raw=true") {
		t.Errorf("last send call = %q, want raw=true (the buddy never needs to decrypt)", last)
	}
}

func TestBuddySendRefusesATruncatedStream(t *testing.T) {
	// Two and a half chunks: the interrupted attempt stores chunk 0 complete and
	// chunk 1 partial, which is exactly what a `zfs send` that died leaves behind.
	payload := bytes.Repeat([]byte("stream-chunk-payload"), (5<<20)/20+1)
	harness := newSenderHarness(t, payload)
	// The dry run promises the whole payload; the send delivers 60% of it.
	harness.agent.truncateTo = len(payload) * 6 / 10

	// Enqueueing still succeeds: the truncation is discovered mid-stream, so the
	// job fails asynchronously and nothing is published.
	jobID := startSend(t, harness, map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/short",
		"receiver": harness.receiverURL,
	})
	failed := waitSend(t, harness, jobID)
	if failed.State != buddyJobFailed {
		t.Fatalf("job = %s, want failed (%s)", failed.State, failed.Error)
	}
	if !strings.Contains(failed.Error, "ended early") {
		t.Errorf("unexpected error: %s", failed.Error)
	}

	// The chain must not be published, so "the latest backup" never points at an
	// incomplete stream - the chunks stay for a retry instead.
	client := buddy.NewClient(harness.receiverURL, harness.identity)
	if _, err := client.Manifest("naslos-test/short", ""); err == nil {
		t.Error("an incomplete chain was published as the current backup")
	}

	// The retry continues the same chain and the same snapshot, so the buddy skips
	// what already arrived instead of storing a second copy of it.
	harness.agent.mu.Lock()
	harness.agent.truncateTo = 0
	harness.agent.mu.Unlock()

	rec := harness.call(t, http.MethodPost, "/api/buddy/send", map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/short",
		"receiver": harness.receiverURL,
	})
	if rec.Code != http.StatusAccepted {
		t.Fatalf("retry enqueue = %d, want 202 (%s)", rec.Code, rec.Body.String())
	}
	var retryStarted struct {
		JobID string `json:"jobId"`
	}
	decode(t, rec, &retryStarted)
	retriedJob := waitSend(t, harness, retryStarted.JobID)
	if retriedJob.State != buddyJobSucceeded || retriedJob.Result == nil {
		t.Fatalf("retry = %s (%s), want succeeded", retriedJob.State, retriedJob.Error)
	}
	retried := *retriedJob.Result

	if !retried.Resumed {
		t.Error("the retry did not report itself as a resume")
	}
	if retried.Skipped == 0 {
		t.Error("the retry re-uploaded everything: the interrupted chain was not continued")
	}
	if retried.Uploaded == 0 {
		t.Error("the retry uploaded nothing even though the stream was incomplete")
	}
	if retried.Snapshot == "" || retried.Chain == "" {
		t.Errorf("retry response = %+v, want the snapshot and chain it continued", retried)
	}

	// And now the chain is complete and restorable.
	var restored bytes.Buffer
	result, err := client.Restore(buddy.RestoreOptions{Source: "naslos-test/short", Out: &restored})
	if err != nil {
		t.Fatalf("restoring the resumed chain: %v", err)
	}
	if !bytes.Equal(restored.Bytes(), payload) {
		t.Error("the resumed chain does not restore to the original stream")
	}
	if result.Manifest.ToSnapshot != retried.Snapshot {
		t.Errorf("manifest snapshot = %q, want the resumed %q", result.Manifest.ToSnapshot, retried.Snapshot)
	}
}

func TestBuddyRestoreStreamsIntoZFS(t *testing.T) {
	payload := bytes.Repeat([]byte("restore-me-"), 400000)
	harness := newSenderHarness(t, payload)
	source := "naslos-test/restore"

	// Back it up twice, so the latest backup is an *incremental*: a restore then has
	// to bring back the full chain first and apply the incremental on top, because
	// ZFS refuses an incremental stream whose base is missing.
	for i := 0; i < 2; i++ {
		sendAndWait(t, harness, "test/data", source)
	}

	rec := harness.call(t, http.MethodPost, "/api/buddy/restore", map[string]any{
		"source":   source,
		"receiver": harness.receiverURL,
		"dataset":  "test/restored",
		"force":    true,
	})
	if rec.Code != http.StatusOK {
		t.Fatalf("restore = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}

	var result struct {
		Status          string   `json:"status"`
		Dataset         string   `json:"dataset"`
		Chains          []string `json:"chains"`
		IncrementalFrom string   `json:"incrementalFrom"`
		Chunks          int      `json:"chunks"`
		PlainBytes      int64    `json:"plainBytes"`
		Kind            string   `json:"kind"`
	}
	decode(t, rec, &result)
	if result.Status != "restored" || result.Dataset != "test/restored" {
		t.Errorf("restore response = %+v, want a restore into test/restored", result)
	}
	if result.PlainBytes != int64(len(payload))*2 {
		t.Errorf("plain bytes = %d, want %d (both chains)", result.PlainBytes, len(payload)*2)
	}
	if result.Kind != "zfs-send" {
		t.Errorf("kind = %q, want zfs-send", result.Kind)
	}
	if len(result.Chains) != 2 {
		t.Fatalf("chains = %v, want the full send and the incremental", result.Chains)
	}
	if result.IncrementalFrom == "" {
		t.Error("the restore did not report the incremental base")
	}

	// Both streams must have reached ZFS, in order, with -F only on the first (a
	// forced incremental would roll the destination back between them).
	harness.agent.mu.Lock()
	received := append([]byte(nil), harness.agent.received.Bytes()...)
	calls := append([]string(nil), harness.agent.receiveCalls...)
	harness.agent.mu.Unlock()

	if want := bytes.Repeat(payload, 2); !bytes.Equal(received, want) {
		t.Errorf("ZFS received %d bytes, want the two streams (%d)", len(received), len(want))
	}
	wantCalls := len(calls)
	if wantCalls != 2 {
		t.Fatalf("receive calls = %v, want two (the full send, then the incremental)", calls)
	}
	if !strings.Contains(calls[0], "force=true") {
		t.Errorf("first receive %q should force: the destination is fresh", calls[0])
	}
	if strings.Contains(calls[1], "force=true") {
		t.Errorf("second receive %q must not force: that would roll the base back", calls[1])
	}
}

func TestBuddyRestoreReportsZFSRefusal(t *testing.T) {
	harness := newSenderHarness(t, []byte("payload"))
	source := "naslos-test/refused"

	sendAndWait(t, harness, "test/data", source)

	// A destination that already has a snapshot refuses the stream; that refusal
	// must reach the operator instead of being reported as success.
	harness.agent.mu.Lock()
	harness.agent.receiveErr = "cannot receive: destination has snapshots"
	harness.agent.mu.Unlock()

	rec := harness.call(t, http.MethodPost, "/api/buddy/restore", map[string]any{
		"source":   source,
		"receiver": harness.receiverURL,
		"dataset":  "test/restored",
	})
	if rec.Code != http.StatusUnprocessableEntity {
		t.Fatalf("restore = %d, want 422 (%s)", rec.Code, rec.Body.String())
	}
	if !strings.Contains(rec.Body.String(), "destination has snapshots") {
		t.Errorf("ZFS's message was dropped: %s", rec.Body.String())
	}
}

func TestBuddyVerifyHashesTheStreamWithoutTouchingZFS(t *testing.T) {
	payload := bytes.Repeat([]byte("verify-me-"), 300000)
	harness := newSenderHarness(t, payload)
	source := "naslos-test/verify"

	sendAndWait(t, harness, "test/data", source)

	// Verify needs no dataset: it decrypts and hashes instead of writing.
	rec := harness.call(t, http.MethodPost, "/api/buddy/restore", map[string]any{
		"source":   source,
		"receiver": harness.receiverURL,
		"verify":   true,
	})
	if rec.Code != http.StatusOK {
		t.Fatalf("verify = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}

	var result struct {
		Status string `json:"status"`
		Chains []struct {
			Chain  string `json:"chain"`
			SHA256 string `json:"sha256"`
			Bytes  int64  `json:"bytes"`
		} `json:"chains"`
		PlainBytes int64 `json:"plainBytes"`
	}
	decode(t, rec, &result)

	if result.Status != "verified" || len(result.Chains) != 1 {
		t.Fatalf("verify response = %+v, want one verified chain", result)
	}
	// The decisive assertion: the digest the instance computes from what the buddy
	// stored equals the digest of the stream the agent produced. Anything the
	// receiver changed - a flipped byte, a reordered chunk, a swapped chain - would
	// break this equality.
	want := fmt.Sprintf("%x", sha256.Sum256(payload))
	if result.Chains[0].SHA256 != want {
		t.Errorf("verified digest = %s, want %s", result.Chains[0].SHA256, want)
	}
	if result.Chains[0].Bytes != int64(len(payload)) || result.PlainBytes != int64(len(payload)) {
		t.Errorf("verified %d/%d bytes, want %d", result.Chains[0].Bytes, result.PlainBytes, len(payload))
	}

	// Verify must not touch the node's ZFS: no `zfs receive` was started.
	harness.agent.mu.Lock()
	calls := append([]string(nil), harness.agent.receiveCalls...)
	harness.agent.mu.Unlock()
	if len(calls) != 0 {
		t.Errorf("verify started zfs receive: %v", calls)
	}
}

func TestBuddySendValidatesInput(t *testing.T) {
	harness := newSenderHarness(t, []byte("payload"))

	cases := []struct {
		name    string
		request map[string]any
	}{
		{"no dataset", map[string]any{"source": "a/b", "receiver": harness.receiverURL}},
		{"no source", map[string]any{"dataset": "test/data", "receiver": harness.receiverURL}},
		{"traversal in the source", map[string]any{"dataset": "test/data", "source": "../etc", "receiver": harness.receiverURL}},
		{"no receiver", map[string]any{"dataset": "test/data", "source": "a/b"}},
		{"not a URL", map[string]any{"dataset": "test/data", "source": "a/b", "receiver": "buddy-nas"}},
		{"wrong scheme", map[string]any{"dataset": "test/data", "source": "a/b", "receiver": "ftp://buddy"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			rec := harness.call(t, http.MethodPost, "/api/buddy/send", tc.request)
			if rec.Code != http.StatusBadRequest {
				t.Errorf("status = %d, want 400 (%s)", rec.Code, rec.Body.String())
			}
		})
	}

	// Without an identity there is nothing to authenticate with, and the error says
	// how to make one.
	t.Setenv("BUDDY_IDENTITY", filepath.Join(t.TempDir(), "missing.json"))
	rec := harness.call(t, http.MethodPost, "/api/buddy/send", map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/test",
		"receiver": harness.receiverURL,
	})
	if rec.Code != http.StatusPreconditionFailed {
		t.Fatalf("status = %d, want 412 (%s)", rec.Code, rec.Body.String())
	}
	if !strings.Contains(rec.Body.String(), "/api/buddy/identity") {
		t.Errorf("the error does not tell the operator how to create an identity: %s", rec.Body.String())
	}
}
