package agent

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
)

// Backup streaming (FR-BUD). The control-plane client is deliberately not used
// here: its DefaultTimeout is 180 s because a control-plane call should never hang,
// but a `zfs send` of a real dataset runs far longer. These calls have no
// client-side total timeout and are bounded by the caller's context and the
// receiver's pace instead. That timeout is exactly why an instance could not back
// itself up before this existed.

// SendStreamOptions mirrors the agent's `zfs send` parameters.
type SendStreamOptions struct {
	// Dataset is the dataset to send (`pool/name`).
	Dataset string
	// To is the snapshot to send, without the `dataset@` prefix.
	To string
	// From is the optional base snapshot (incremental sends).
	From string
	// Raw sends the encrypted records as they are (-w).
	Raw bool
}

// SnapshotInfo mirrors the agent's snapshot identity: the name plus the GUID that
// ties a snapshot to the stream a buddy stored.
type SnapshotInfo struct {
	Name    string `json:"name"`
	GUID    string `json:"guid"`
	Created string `json:"created"`
}

// streamClient has no total timeout: streaming calls are bounded by their context.
var streamClient = &http.Client{}

// SendStream starts a `zfs send` on the node and returns its stdout as an
// io.ReadCloser, which the caller must close. A send that dies in the middle cannot
// change an already-sent status code, so completeness is established the same way
// on both sides: the caller compares the bytes it received against EstimateSend,
// and the far side's `zfs receive` refuses a truncated stream.
func (c *Client) SendStream(ctx context.Context, opts SendStreamOptions) (io.ReadCloser, error) {
	target := c.baseURL + "/api/v1/zfs/send/" + escapeDatasetPath(opts.Dataset) + "?" + sendQuery(opts, false)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return nil, fmt.Errorf("creating send request: %w", err)
	}

	resp, err := streamClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("contacting agent at %s: %w", c.baseURL, err)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		defer resp.Body.Close()
		return nil, readAgentError(resp)
	}
	return resp.Body, nil
}

// EstimateSend asks the node how many bytes a send would produce (a dry run), so a
// sender can show progress and detect a truncated stream afterwards.
func (c *Client) EstimateSend(ctx context.Context, opts SendStreamOptions) (int64, error) {
	target := c.baseURL + "/api/v1/zfs/send/" + escapeDatasetPath(opts.Dataset) + "?" + sendQuery(opts, true)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return 0, fmt.Errorf("creating estimate request: %w", err)
	}

	var payload struct {
		Bytes int64 `json:"bytes"`
	}
	if err := c.do(req, &payload); err != nil {
		return 0, err
	}
	return payload.Bytes, nil
}

// sendQuery builds the query string shared by the send and estimate calls.
func sendQuery(opts SendStreamOptions, estimate bool) string {
	query := url.Values{}
	query.Set("to", opts.To)
	if opts.From != "" {
		query.Set("from", opts.From)
	}
	query.Set("raw", strconv.FormatBool(opts.Raw))
	if estimate {
		query.Set("estimate", "true")
	}
	return query.Encode()
}

// ReceiveStream pipes a restore into `zfs receive` on the node. It returns the
// writer to fill and a waiter: the waiter must be called after the writer is closed,
// and it reports what ZFS said (a truncated stream or one the destination already
// has fails there, which is the strongest check a restore has).
func (c *Client) ReceiveStream(ctx context.Context, dataset string, force bool) (io.WriteCloser, func() error, error) {
	target := c.baseURL + "/api/v1/zfs/receive/" + escapeDatasetPath(dataset)
	if force {
		target += "?force=true"
	}

	reader, writer := io.Pipe()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, target, reader)
	if err != nil {
		return nil, nil, fmt.Errorf("creating receive request: %w", err)
	}
	req.Header.Set("Content-Type", "application/octet-stream")

	done := make(chan error, 1)
	go func() {
		resp, err := streamClient.Do(req)
		if err != nil {
			done <- fmt.Errorf("contacting agent at %s: %w", c.baseURL, err)
			return
		}
		defer resp.Body.Close()

		body, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			done <- &Error{Status: resp.StatusCode, Message: agentErrorMessage(body)}
			return
		}
		done <- nil
	}()

	wait := func() error {
		// Closing the pipe ends the request body, so `zfs receive` sees
		// end-of-stream and can finish (or refuse) instead of waiting forever.
		closeErr := writer.Close()
		waitErr := <-done
		if closeErr != nil {
			return closeErr
		}
		return waitErr
	}
	return writer, wait, nil
}

// SnapshotsWithGUID lists a dataset's snapshots with their GUIDs: how the API
// recognises the snapshot it sent last time (by comparing the manifest's ToGUID) and
// therefore whether the next send can be incremental.
func (c *Client) SnapshotsWithGUID(ctx context.Context, dataset string) ([]SnapshotInfo, error) {
	target := c.baseURL + "/api/v1/zfs/snapshots/" + escapeDatasetPath(dataset)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return nil, fmt.Errorf("creating snapshots request: %w", err)
	}

	var snapshots []SnapshotInfo
	if err := c.do(req, &snapshots); err != nil {
		return nil, err
	}
	if snapshots == nil {
		snapshots = []SnapshotInfo{}
	}
	return snapshots, nil
}

// CreateSnapshot snapshots a dataset on the node (`zfs snapshot ds@name`).
func (c *Client) CreateSnapshot(ctx context.Context, dataset, name string) error {
	body, err := json.Marshal(map[string]string{"name": name})
	if err != nil {
		return fmt.Errorf("encoding snapshot request: %w", err)
	}

	target := c.baseURL + "/api/v1/snapshots/" + escapeDatasetPath(dataset)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, target, strings.NewReader(string(body)))
	if err != nil {
		return fmt.Errorf("creating snapshot request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	return c.do(req, nil)
}

// readAgentError turns a failed streaming response into an *Error.
func readAgentError(resp *http.Response) error {
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	return &Error{Status: resp.StatusCode, Message: agentErrorMessage(body)}
}

// agentErrorMessage extracts {"error": "…"} from an agent body.
func agentErrorMessage(body []byte) string {
	var payload map[string]string
	if json.Unmarshal(body, &payload) == nil && payload["error"] != "" {
		return payload["error"]
	}
	text := strings.TrimSpace(string(body))
	if text == "" {
		text = "the agent returned no message"
	}
	return text
}
