package server

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/AessemOps/Naslos-Linux/agent/internal/zfs"
)

// fakeBackup stands in for the ZFS client: the handlers' contract is validation
// and streaming, and both can be checked without a pool.
type fakeBackup struct {
	sendArgs    []string
	sendBody    string
	sendErr     error
	estimate    int64
	estimateErr error
	receiveArgs []string
	received    bytes.Buffer
	// receiveErr fails the call itself (what validation does); receiveWaitErr
	// fails the command *after* it accepted the stream, which is how ZFS reports
	// a truncated or invalid stream.
	receiveErr     error
	receiveWaitErr error
	snapshots      []zfs.SnapshotInfo
	snapshotErr    error
}

func (f *fakeBackup) SendStream(opts zfs.SendStreamOptions) (io.ReadCloser, func() error, error) {
	f.sendArgs = []string{opts.Dataset, opts.To, opts.From}
	if f.sendErr != nil {
		return nil, nil, f.sendErr
	}
	return io.NopCloser(strings.NewReader(f.sendBody)), func() error { return nil }, nil
}

func (f *fakeBackup) EstimateSend(opts zfs.SendStreamOptions) (int64, error) {
	f.sendArgs = []string{opts.Dataset, opts.To, opts.From}
	return f.estimate, f.estimateErr
}

func (f *fakeBackup) ReceiveStream(dataset string, force bool) (io.WriteCloser, func() error, error) {
	f.receiveArgs = []string{dataset, map[bool]string{true: "force", false: ""}[force]}
	if f.receiveErr != nil {
		return nil, nil, f.receiveErr
	}
	return &fakeStdin{target: &f.received}, func() error { return f.receiveWaitErr }, nil
}

// fakeStdin is the receiving end of a faked `zfs receive`.
type fakeStdin struct {
	target *bytes.Buffer
}

func (f *fakeStdin) Write(p []byte) (int, error) { return f.target.Write(p) }
func (f *fakeStdin) Close() error                { return nil }

// fakeError is an arbitrary failure for the error paths.
type fakeError struct{ msg string }

func (e *fakeError) Error() string { return e.msg }

// newTestServer builds a server with the fake streaming client wired in. A nil
// client means a degraded agent (no ZFS on the host). The token check is off:
// these tests exercise the handlers, and the gate itself is covered by
// TestRequireAuth.
func newTestServer(backup backupZFS) *Server {
	s := &Server{router: http.NewServeMux(), authDisabled: true}
	if backup != nil {
		s.backup = backup
		s.zfs = &zfs.Client{}
	}
	s.routes()
	return s
}

func doRequest(t *testing.T, s *Server, method, path string, body io.Reader) *httptest.ResponseRecorder {
	t.Helper()

	req := httptest.NewRequest(method, path, body)
	rec := httptest.NewRecorder()
	s.router.ServeHTTP(rec, req)
	return rec
}

func TestSendStreamHandlerStreamsTheStream(t *testing.T) {
	fake := &fakeBackup{sendBody: "zfs-send-payload"}
	rec := doRequest(t, newTestServer(fake), http.MethodGet, "/api/v1/zfs/send/tank/data?to=buddy-1&from=buddy-0", nil)

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	if got := rec.Body.String(); got != "zfs-send-payload" {
		t.Errorf("body = %q, want the stream", got)
	}
	if got := rec.Header().Get("Content-Type"); got != "application/octet-stream" {
		t.Errorf("content type = %q, want application/octet-stream", got)
	}
	if want := "tank/data|buddy-1|buddy-0"; strings.Join(fake.sendArgs, "|") != want {
		t.Errorf("send called with %v, want %s", fake.sendArgs, want)
	}
}

func TestSendStreamHandlerReportsRefusals(t *testing.T) {
	cases := []struct {
		name string
		path string
	}{
		{"no target snapshot", "/api/v1/zfs/send/tank/data"},
		{"rejected by the zfs layer", "/api/v1/zfs/send/tank/data?to=-R"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			fake := &fakeBackup{sendBody: "should-not-be-streamed", sendErr: &fakeError{"invalid input"}}
			rec := doRequest(t, newTestServer(fake), http.MethodGet, tc.path, nil)
			if rec.Code != http.StatusBadRequest {
				t.Errorf("status = %d, want 400 (%s)", rec.Code, rec.Body.String())
			}
			if strings.Contains(rec.Body.String(), "should-not-be-streamed") {
				t.Error("a refused request streamed data")
			}
			if !strings.Contains(rec.Body.String(), "error") {
				t.Errorf("refusal was not reported as JSON: %s", rec.Body.String())
			}
		})
	}
}

func TestSendStreamEstimate(t *testing.T) {
	fake := &fakeBackup{estimate: 4294967296}
	rec := doRequest(t, newTestServer(fake), http.MethodGet, "/api/v1/zfs/send/tank/data?to=buddy-1&estimate=true", nil)

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", rec.Code)
	}
	var payload struct {
		Bytes int64  `json:"bytes"`
		To    string `json:"to"`
		Raw   bool   `json:"raw"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &payload); err != nil {
		t.Fatalf("decoding estimate: %v", err)
	}
	if payload.Bytes != 4294967296 {
		t.Errorf("bytes = %d, want 4294967296", payload.Bytes)
	}
	if payload.To != "buddy-1" || !payload.Raw {
		t.Errorf("estimate payload = %+v, want to=buddy-1 raw=true", payload)
	}
}

// SnapshotsWithGUID completes the backupZFS interface for the fake.
func (f *fakeBackup) SnapshotsWithGUID(dataset string) ([]zfs.SnapshotInfo, error) {
	if f.snapshotErr != nil {
		return nil, f.snapshotErr
	}
	return f.snapshots, nil
}

func TestReceiveStreamHandlerPipesTheBody(t *testing.T) {
	fake := &fakeBackup{}
	rec := doRequest(t, newTestServer(fake), http.MethodPost, "/api/v1/zfs/receive/tank/restored?force=true", strings.NewReader("restore-stream"))

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	if got := fake.received.String(); got != "restore-stream" {
		t.Errorf("receive got %q, want the request body", got)
	}
	if want := "tank/restored|force"; strings.Join(fake.receiveArgs, "|") != want {
		t.Errorf("receive called with %v, want %s", fake.receiveArgs, want)
	}

	var payload struct {
		Dataset string `json:"dataset"`
		Bytes   int64  `json:"bytes"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &payload); err != nil {
		t.Fatalf("decoding receive response: %v", err)
	}
	if payload.Bytes != int64(len("restore-stream")) {
		t.Errorf("bytes = %d, want %d", payload.Bytes, len("restore-stream"))
	}
	if payload.Dataset != "tank/restored" {
		t.Errorf("dataset = %q, want tank/restored", payload.Dataset)
	}
}

func TestReceiveStreamHandlerReportsZFSErrors(t *testing.T) {
	// ZFS refuses a truncated stream itself; its message must survive the trip,
	// because that message is how an operator learns the restore was incomplete.
	fake := &fakeBackup{receiveWaitErr: &fakeError{"cannot receive: destination has snapshots"}}
	rec := doRequest(t, newTestServer(fake), http.MethodPost, "/api/v1/zfs/receive/tank/restored", strings.NewReader("stream"))

	if rec.Code != http.StatusUnprocessableEntity {
		t.Errorf("status = %d, want 422 (%s)", rec.Code, rec.Body.String())
	}
	if !strings.Contains(rec.Body.String(), "destination has snapshots") {
		t.Errorf("ZFS's own message was dropped: %s", rec.Body.String())
	}
}

func TestReceiveStreamHandlerReportsRefusedStart(t *testing.T) {
	// Validation lives in the zfs package (dataset shape, no flags); the handler
	// must forward its refusal instead of pretending to have restored something.
	fake := &fakeBackup{receiveErr: &fakeError{"dataset \"nested\" must be <pool>/<name>"}}
	rec := doRequest(t, newTestServer(fake), http.MethodPost, "/api/v1/zfs/receive/nested", strings.NewReader("stream"))

	if rec.Code != http.StatusBadRequest {
		t.Errorf("status = %d, want 400 (%s)", rec.Code, rec.Body.String())
	}
	if fake.received.Len() != 0 {
		t.Error("a refused receive consumed the stream")
	}
}

func TestBackupSnapshotsHandler(t *testing.T) {
	fake := &fakeBackup{snapshots: []zfs.SnapshotInfo{{Name: "buddy-1", GUID: "222", Created: "1700000100"}}}
	rec := doRequest(t, newTestServer(fake), http.MethodGet, "/api/v1/zfs/snapshots/tank/data", nil)

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	if body := rec.Body.String(); !strings.Contains(body, `"guid":"222"`) {
		t.Errorf("snapshot GUID missing from %s", body)
	}
}

func TestBackupEndpointsRefuseWhenZFSIsUnavailable(t *testing.T) {
	// A degraded agent (no ZFS on the host) must say so instead of streaming
	// nothing, which a sender would otherwise record as an empty backup.
	for _, path := range []string{"/api/v1/zfs/send/tank/data?to=s1", "/api/v1/zfs/snapshots/tank/data"} {
		rec := doRequest(t, newTestServer(nil), http.MethodGet, path, nil)
		if rec.Code != http.StatusServiceUnavailable {
			t.Errorf("%s: status = %d, want 503", path, rec.Code)
		}
	}
	rec := doRequest(t, newTestServer(nil), http.MethodPost, "/api/v1/zfs/receive/tank/x", strings.NewReader(""))
	if rec.Code != http.StatusServiceUnavailable {
		t.Errorf("receive: status = %d, want 503", rec.Code)
	}
}

func TestBackupEndpointsRejectWrongMethod(t *testing.T) {
	fake := &fakeBackup{}
	if rec := doRequest(t, newTestServer(fake), http.MethodPost, "/api/v1/zfs/send/tank/data?to=s1", nil); rec.Code != http.StatusMethodNotAllowed {
		t.Errorf("POST /zfs/send = %d, want 405", rec.Code)
	}
	if rec := doRequest(t, newTestServer(fake), http.MethodGet, "/api/v1/zfs/receive/tank/x", nil); rec.Code != http.StatusMethodNotAllowed {
		t.Errorf("GET /zfs/receive = %d, want 405", rec.Code)
	}
}
