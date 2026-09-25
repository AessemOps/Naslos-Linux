package server

import (
	"io"
	"log"
	"net/http"
	"strings"
	"time"

	"github.com/AessemOps/Naslos-Linux/agent/internal/zfs"
)

// backupZFS is the slice of the ZFS client the backup handlers use. It is an
// interface so the handlers can be tested without a host: their job is validation
// and streaming, and both deserve tests that do not need a pool.
type backupZFS interface {
	SendStream(opts zfs.SendStreamOptions) (io.ReadCloser, func() error, error)
	EstimateSend(opts zfs.SendStreamOptions) (int64, error)
	ReceiveStream(dataset string, force bool) (io.WriteCloser, func() error, error)
	SnapshotsWithGUID(dataset string) ([]zfs.SnapshotInfo, error)
}

// handleSendStream streams a `zfs send` to the caller:
//
//	GET /api/v1/zfs/send/{dataset}?to=<snap>[&from=<base>][&raw=false][&estimate=true]
//
// The dataset stays in the path because it contains slashes; the snapshot names
// are validated before they reach a command line (SEC-1: the agent never passes
// client input through to a shell, and these are the only backup arguments that
// exist).
//
// With estimate=true it answers a JSON dry run instead of a stream, so the API can
// show progress and, more importantly, recognise a truncated stream later.
func (s *Server) handleSendStream(w http.ResponseWriter, r *http.Request) {
	if s.backupUnavailable(w) {
		return
	}
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	query := r.URL.Query()
	opts := zfs.SendStreamOptions{
		Dataset: strings.TrimPrefix(r.URL.Path, "/api/v1/zfs/send/"),
		To:      query.Get("to"),
		From:    query.Get("from"),
		// -w is the default: a backup wants the encrypted records, not a
		// decrypted copy the receiver could read.
		Raw: query.Get("raw") != "false",
	}
	if opts.To == "" {
		writeError(w, http.StatusBadRequest, "to is required (the snapshot to send, without the dataset@ prefix)")
		return
	}

	if query.Get("estimate") == "true" {
		bytes, err := s.backup.EstimateSend(opts)
		if err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]any{
			"dataset": opts.Dataset,
			"to":      opts.To,
			"from":    opts.From,
			"raw":     opts.Raw,
			"bytes":   bytes,
		})
		return
	}

	stream, wait, err := s.backup.SendStream(opts)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	defer stream.Close()

	w.Header().Set("Content-Type", "application/octet-stream")
	w.WriteHeader(http.StatusOK)

	written, copyErr := io.Copy(w, stream)
	if copyErr != nil {
		// Headers are already out, so the status cannot change. The API detects a
		// short stream against the estimate it asked for, and `zfs receive`
		// refuses a truncated stream on the far side.
		log.Printf("zfs send %s@%s aborted after %d bytes: %v", opts.Dataset, opts.To, written, copyErr)
		return
	}
	if err := wait(); err != nil {
		log.Printf("zfs send %s@%s failed after %d bytes: %v", opts.Dataset, opts.To, written, err)
		return
	}
	log.Printf("zfs send %s@%s streamed %d bytes (from=%q raw=%v)", opts.Dataset, opts.To, written, opts.From, opts.Raw)
}

// handleReceiveStream streams a `zfs receive`:
//
//	POST /api/v1/zfs/receive/{dataset}?force=true
//
// The body is a `zfs send` stream and is piped straight into ZFS, so a restore
// never needs a temporary copy of the data. force applies -F: it rolls the
// dataset back to the stream's snapshot instead of refusing when the destination
// already has one.
func (s *Server) handleReceiveStream(w http.ResponseWriter, r *http.Request) {
	if s.backupUnavailable(w) {
		return
	}
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	dataset := strings.TrimPrefix(r.URL.Path, "/api/v1/zfs/receive/")
	force := r.URL.Query().Get("force") == "true"

	// This body is a whole `zfs send` stream and can take minutes, so clear the
	// server-wide read deadline for this connection (PF-M6). A failure here just
	// leaves the default deadline in place.
	if rc := http.NewResponseController(w); rc != nil {
		_ = rc.SetReadDeadline(time.Time{})
	}

	stdin, wait, err := s.backup.ReceiveStream(dataset, force)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	written, copyErr := io.Copy(stdin, r.Body)
	// Closing stdin lets `zfs receive` see end-of-stream; without it the command
	// waits forever and the dataset stays incomplete.
	closeErr := stdin.Close()
	if copyErr != nil {
		_ = wait()
		writeError(w, http.StatusBadRequest, "reading the restore stream: "+copyErr.Error())
		return
	}
	if closeErr != nil {
		_ = wait()
		writeError(w, http.StatusInternalServerError, "closing the restore stream: "+closeErr.Error())
		return
	}
	if err := wait(); err != nil {
		writeError(w, http.StatusUnprocessableEntity, err.Error())
		return
	}

	log.Printf("zfs receive %s restored %d bytes (force=%v)", dataset, written, force)
	writeJSON(w, http.StatusOK, map[string]any{
		"status":  "received",
		"dataset": dataset,
		"bytes":   written,
		"force":   force,
	})
}

// handleBackupSnapshots lists a dataset's snapshots with their GUIDs, which is how
// the API decides whether the next send can be incremental:
//
//	GET /api/v1/zfs/snapshots/{dataset}
func (s *Server) handleBackupSnapshots(w http.ResponseWriter, r *http.Request) {
	if s.backupUnavailable(w) {
		return
	}
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	dataset := strings.TrimPrefix(r.URL.Path, "/api/v1/zfs/snapshots/")
	snapshots, err := s.backup.SnapshotsWithGUID(dataset)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, snapshots)
}
