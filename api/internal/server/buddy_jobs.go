package server

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/agent"
	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
	"github.com/AessemOps/Naslos-Linux/api/internal/notifications"
)

// Async send jobs (FR-BUD-15/16).
//
// POST /api/buddy/send used to stream a whole `zfs send` inside the request:
// when the operator's client went away the send died with it. Jobs decouple the
// two: the handler validates, enqueues, and answers 202 immediately, while the
// send runs under a server-owned context the operator can poll and cancel.
//
// Jobs live in memory; the receiver's manifests are the durable record. The
// resume-state file on disk is what makes a cancelled or killed send resumable,
// exactly as before.

// buddyJobState is the lifecycle of one send job.
type buddyJobState string

const (
	buddyJobRunning   buddyJobState = "running"
	buddyJobSucceeded buddyJobState = "succeeded"
	buddyJobFailed    buddyJobState = "failed"
	buddyJobCancelled buddyJobState = "cancelled"
)

// buddySendResult is the payload a finished send reports: the fields
// handleBuddySend used to return synchronously.
type buddySendResult struct {
	Status         string  `json:"status"`
	Dataset        string  `json:"dataset"`
	Source         string  `json:"source"`
	Receiver       string  `json:"receiver"`
	Snapshot       string  `json:"snapshot"`
	Base           string  `json:"base,omitempty"`
	BaseGUID       string  `json:"baseGUID,omitempty"`
	ToGUID         string  `json:"toGUID,omitempty"`
	Incremental    bool    `json:"incremental"`
	Resumed        bool    `json:"resumed"`
	Raw            bool    `json:"raw"`
	Chain          string  `json:"chain"`
	Chunks         int     `json:"chunks"`
	Uploaded       int     `json:"uploaded"`
	Skipped        int     `json:"skipped"`
	PlainBytes     int64   `json:"plainBytes"`
	SealedBytes    int64   `json:"sealedBytes"`
	EstimatedBytes int64   `json:"estimatedBytes,omitempty"`
	PrunedChains   int     `json:"prunedChains,omitempty"`
	DurationSecs   float64 `json:"durationSeconds"`
}

// buddyJobPublic is the serializable snapshot of one send job: everything the
// API reports, without the mutex, context or cancel func.
type buddyJobPublic struct {
	ID         string        `json:"id"`
	Dataset    string        `json:"dataset"`
	Source     string        `json:"source"`
	Receiver   string        `json:"receiver"`
	PruneKeep  int           `json:"pruneKeep,omitempty"`
	Raw        bool          `json:"raw"`
	Force      bool          `json:"force,omitempty"`
	ScheduleID string        `json:"scheduleId,omitempty"`
	State      buddyJobState `json:"state"`
	StartedAt  time.Time     `json:"startedAt"`
	FinishedAt time.Time     `json:"finishedAt,omitempty"`
	// Progress is the live snapshot wired to PushOptions.Progress.
	Progress    buddy.PushProgress `json:"progress"`
	Incremental bool               `json:"incremental"`
	Resumed     bool               `json:"resumed"`
	Snapshot    string             `json:"snapshot,omitempty"`
	Estimated   int64              `json:"estimatedBytes,omitempty"`
	Result      *buddySendResult   `json:"result,omitempty"`
	Error       string             `json:"error,omitempty"`
}

// buddyJob is one async send.
type buddyJob struct {
	buddyJobPublic

	mu     sync.Mutex
	ctx    context.Context
	cancel context.CancelFunc
	done   chan struct{}
}

// snapshot returns a copy safe to serialize.
func (j *buddyJob) snapshot() buddyJobPublic {
	j.mu.Lock()
	defer j.mu.Unlock()
	return j.buddyJobPublic
}

// buddyJobManager keeps running jobs plus the last ~20 finished ones.
type buddyJobManager struct {
	mu    sync.Mutex
	jobs  map[string]*buddyJob
	order []string
}

func newBuddyJobManager() *buddyJobManager {
	return &buddyJobManager{jobs: make(map[string]*buddyJob)}
}

// randomJobID returns a 128-bit hex job id. 32 bits (the original 8 hex
// characters) is small enough to be worth guessing for a caller who can already
// reach the API, and job ids are the handle for cancelling or inspecting someone
// else's transfer (NAS-014).
func randomJobID() string {
	buf := make([]byte, 16)
	if _, err := rand.Read(buf); err != nil {
		return fmt.Sprintf("%032x", time.Now().UnixNano())
	}
	return hex.EncodeToString(buf)
}

// conflicting returns a running job that shares the resume-state file (same
// receiver+source). Two jobs may target the same dataset, which is what lets one
// fan-out run back the same dataset up to several buddies at once: each job
// snapshots under its own unique name and streams that snapshot, so they do not
// race on the snapshot itself.
func (m *buddyJobManager) conflicting(receiver, source string) *buddyJob {
	m.mu.Lock()
	defer m.mu.Unlock()
	for _, job := range m.jobs {
		job.mu.Lock()
		running := job.State == buddyJobRunning
		samePair := job.Receiver == receiver && job.Source == source
		job.mu.Unlock()
		if running && samePair {
			return job
		}
	}
	return nil
}

func (m *buddyJobManager) add(job *buddyJob) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.jobs[job.ID] = job
	m.order = append(m.order, job.ID)
	// Keep running jobs plus the last ~20 finished ones.
	kept := make([]string, 0, len(m.order))
	finished := 0
	for i := len(m.order) - 1; i >= 0; i-- {
		j := m.jobs[m.order[i]]
		if j == nil {
			continue
		}
		j.mu.Lock()
		state := j.State
		j.mu.Unlock()
		if state == buddyJobRunning || finished < 20 {
			kept = append(kept, m.order[i])
			if state != buddyJobRunning {
				finished++
			}
		}
	}
	for i, j := 0, len(kept)-1; i < j; i, j = i+1, j-1 {
		kept[i], kept[j] = kept[j], kept[i]
	}
	for id, j := range m.jobs {
		found := false
		for _, k := range kept {
			if k == id {
				found = true
				break
			}
		}
		if !found {
			delete(m.jobs, id)
		}
		_ = j
	}
	m.order = kept
}

func (m *buddyJobManager) get(id string) *buddyJob {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.jobs[id]
}

func (m *buddyJobManager) list() []buddyJobPublic {
	m.mu.Lock()
	ids := append([]string(nil), m.order...)
	m.mu.Unlock()
	out := make([]buddyJobPublic, 0, len(ids))
	for _, id := range ids {
		m.mu.Lock()
		job := m.jobs[id]
		m.mu.Unlock()
		if job != nil {
			out = append(out, job.snapshot())
		}
	}
	return out
}

func (m *buddyJobManager) cancelAll() {
	m.mu.Lock()
	jobs := make([]*buddyJob, 0, len(m.jobs))
	for _, j := range m.jobs {
		jobs = append(jobs, j)
	}
	m.mu.Unlock()
	for _, j := range jobs {
		j.mu.Lock()
		running := j.State == buddyJobRunning
		cancel := j.cancel
		j.mu.Unlock()
		if running && cancel != nil {
			cancel()
		}
	}
}

// ensureBuddyJobs lazily creates the manager (test harnesses build Server by hand).
func (s *Server) ensureBuddyJobs() *buddyJobManager {
	if s.buddyJobs == nil {
		s.buddyJobs = newBuddyJobManager()
	}
	return s.buddyJobs
}

// handleBuddySend validates and enqueues an async send:
//
//	POST /api/buddy/send {"dataset","source","receiver",...} → 202 {"jobId","status":"started"}
func (s *Server) handleBuddySend(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if s.agent == nil {
		writeError(w, http.StatusServiceUnavailable, "the API has no agent client, so it cannot stream ZFS data")
		return
	}

	var request buddySendRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, req.Body, 64<<10)).Decode(&request); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request: "+err.Error())
		return
	}

	dataset := strings.TrimSpace(request.Dataset)
	source := strings.TrimSpace(request.Source)
	if dataset == "" || source == "" {
		writeError(w, http.StatusBadRequest, "dataset and source are required")
		return
	}
	if err := buddy.ValidateSource(source); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	receiverURL, err := normalizeReceiverURL(request.Receiver)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	if _, err := loadBuddyIdentity(); err != nil {
		writeError(w, http.StatusPreconditionFailed, err.Error())
		return
	}

	jobs := s.ensureBuddyJobs()
	if conflict := jobs.conflicting(receiverURL, source); conflict != nil {
		c := conflict.snapshot()
		writeError(w, http.StatusConflict, fmt.Sprintf(
			"a backup of this source is already running (job %s, started %s)",
			c.ID, c.StartedAt.Format(time.RFC3339)))
		return
	}

	raw := true
	if request.Raw != nil {
		raw = *request.Raw
	}
	ctx, cancel := context.WithCancel(context.Background())
	job := &buddyJob{
		buddyJobPublic: buddyJobPublic{
			ID:        randomJobID(),
			Dataset:   dataset,
			Source:    source,
			Receiver:  receiverURL,
			PruneKeep: request.PruneKeep,
			Raw:       raw,
			Force:     request.Force,
			State:     buddyJobRunning,
			StartedAt: time.Now().UTC(),
		},
		ctx:    ctx,
		cancel: cancel,
		done:   make(chan struct{}),
	}
	jobs.add(job)
	go s.runBuddySendJob(job)
	writeJSON(w, http.StatusAccepted, map[string]any{"jobId": job.ID, "status": "started"})
}

// runBuddySendJob executes the send the handler used to run inline. The context is
// the job's own: a disconnected operator no longer kills the stream. A cancelled
// or killed send keeps its resume-state file, so a retry continues the same chain.
func (s *Server) runBuddySendJob(job *buddyJob) {
	started := time.Now()
	defer close(job.done)

	setProgress := func(p buddy.PushProgress) {
		job.mu.Lock()
		job.Progress = p
		job.mu.Unlock()
	}
	fail := func(msg string) {
		job.mu.Lock()
		if job.ctx.Err() == context.Canceled && job.State == buddyJobRunning {
			job.State = buddyJobCancelled
			job.Error = "cancelled"
		} else {
			job.State = buddyJobFailed
			job.Error = msg
		}
		job.FinishedAt = time.Now().UTC()
		job.mu.Unlock()
		s.afterBuddyJob(job)
	}

	ctx := job.ctx
	job.mu.Lock()
	dataset, source, receiverURL := job.Dataset, job.Source, job.Receiver
	pruneKeep, raw, force := job.PruneKeep, job.Raw, job.Force
	job.mu.Unlock()

	if s.agent == nil {
		fail("the API has no agent client, so it cannot stream ZFS data")
		return
	}

	identity, err := loadBuddyIdentity()
	if err != nil {
		fail(err.Error())
		return
	}
	client := buddy.NewClient(receiverURL, identity)

	statePath := filepath.Join(sendStateDir(), sendStateName(receiverURL, source))
	var state *instanceSendState
	wasResume := false
	if !force {
		if loaded, err := loadSendState(statePath); err == nil &&
			loaded.Receiver == receiverURL && loaded.Source == source && loaded.Dataset == dataset {
			if snapshots, err := s.agent.SnapshotsWithGUID(ctx, dataset); err == nil {
				for _, snapshot := range snapshots {
					if snapshot.Name == loaded.ToSnapshot {
						state = loaded
						wasResume = true
						break
					}
				}
			}
			if state == nil {
				log.Printf("buddy send: discarding the resume state for %s: snapshot %s no longer exists", source, loaded.ToSnapshot)
				_ = os.Remove(statePath)
			}
		}
	}

	base, baseGUID := "", ""
	if state != nil {
		base, baseGUID = state.FromSnapshot, state.FromGUID
	} else if !force {
		previous, err := client.ManifestContext(ctx, source, "")
		if err == nil && previous.Kind == "zfs-send" && previous.ToGUID != "" {
			if snapshots, err := s.agent.SnapshotsWithGUID(ctx, dataset); err == nil {
				for _, snapshot := range snapshots {
					if snapshot.GUID == previous.ToGUID {
						base, baseGUID = snapshot.Name, snapshot.GUID
						break
					}
				}
			}
		} else if err != nil {
			log.Printf("buddy send: no previous backup of %s on %s (%v): sending a full stream", source, receiverURL, err)
		}
	}

	// Refuse a dataset the host namespace cannot see, before snapshotting
	// anything: `zfs send` would capture an empty filesystem and the job would
	// still report success (the mount-propagation trap).
	if err := s.requireMountedDataset(dataset); err != nil {
		fail("refusing to back up: " + err.Error())
		return
	}

	snapshot := "buddy-" + time.Now().UTC().Format("20060102T150405Z") + "-" + randomSuffix()
	targetGUID := ""
	if state != nil {
		snapshot, targetGUID = state.ToSnapshot, state.ToGUID
	}
	job.mu.Lock()
	// A resumed attempt reuses the snapshot it started with, so it is already
	// real; a fresh attempt reports its snapshot after creating it below.
	if state != nil {
		job.Snapshot = snapshot
	}
	job.Resumed = wasResume
	job.Incremental = base != ""
	job.mu.Unlock()

	if state == nil {
		if err := s.agent.CreateSnapshot(ctx, dataset, snapshot); err != nil {
			fail("snapshotting " + dataset + ": " + err.Error())
			return
		}
		// Report the snapshot only once it exists: the job used to name the
		// proposed snapshot before creating it, so a failed attempt showed a
		// snapshot that was never taken.
		job.mu.Lock()
		job.Snapshot = snapshot
		job.mu.Unlock()
		if snapshots, err := s.agent.SnapshotsWithGUID(ctx, dataset); err == nil {
			for _, info := range snapshots {
				if info.Name == snapshot {
					targetGUID = info.GUID
				}
			}
		}
		state = &instanceSendState{
			Receiver:     receiverURL,
			Source:       source,
			Dataset:      dataset,
			ToSnapshot:   snapshot,
			FromSnapshot: base,
			FromGUID:     baseGUID,
			ToGUID:       targetGUID,
		}
		if chain, err := buddy.NewChainState(source, "zfs-send"); err != nil {
			fail("creating the chain state: " + err.Error())
			return
		} else {
			state.Chain = *chain
		}
	}
	if err := saveSendState(statePath, state); err != nil {
		fail("saving the resume state: " + err.Error())
		return
	}

	sendOptions := agent.SendStreamOptions{Dataset: dataset, To: snapshot, From: base, Raw: raw}
	expected, err := s.agent.EstimateSend(ctx, sendOptions)
	if err != nil {
		log.Printf("buddy send: could not estimate %s@%s: %v", dataset, snapshot, err)
	}
	job.mu.Lock()
	job.Estimated = expected
	job.mu.Unlock()

	stream, err := s.agent.SendStream(ctx, sendOptions)
	if err != nil {
		fail("starting zfs send: " + err.Error())
		return
	}
	defer stream.Close()

	chainState := state.Chain
	result, err := client.Push(buddy.PushOptions{
		Source:       source,
		Kind:         "zfs-send",
		Reader:       stream,
		State:        &chainState,
		FromSnapshot: base,
		ToSnapshot:   snapshot,
		FromGUID:     baseGUID,
		ToGUID:       targetGUID,
		PruneKeep:    pruneKeep,
		BeforePublish: func(pushed *buddy.PushResult) error {
			if expected > 0 && pushed.PlainBytes+sendShortfallAllowance(expected) < expected {
				return fmt.Errorf("the send stream ended early (%d of at least %d bytes), so nothing was published. "+
					"Retry the same send to continue chain %s: the buddy skips the chunks it already has",
					pushed.PlainBytes, expected, chainState.Chain)
			}
			return nil
		},
		Progress: setProgress,
	})
	if err != nil {
		if ctx.Err() == context.Canceled {
			fail("cancelled")
			return
		}
		fail(err.Error())
		return
	}

	_ = os.Remove(statePath)

	res := &buddySendResult{
		Status:         "backed up",
		Dataset:        dataset,
		Source:         source,
		Receiver:       receiverURL,
		Snapshot:       snapshot,
		Base:           base,
		BaseGUID:       baseGUID,
		ToGUID:         targetGUID,
		Incremental:    base != "",
		Resumed:        wasResume,
		Raw:            raw,
		Chain:          result.Chain,
		Chunks:         result.Chunks,
		Uploaded:       result.Uploaded,
		Skipped:        result.Skipped,
		PlainBytes:     result.PlainBytes,
		SealedBytes:    result.SealedBytes,
		EstimatedBytes: expected,
		PrunedChains:   result.PrunedChains,
		DurationSecs:   time.Since(started).Seconds(),
	}
	log.Printf("buddy send: %s@%s -> %s %s (%d chunks, %d bytes, incremental=%v, resumed=%v)",
		dataset, snapshot, receiverURL, source, result.Chunks, result.PlainBytes, base != "", wasResume)
	job.mu.Lock()
	job.State = buddyJobSucceeded
	job.Result = res
	job.Progress = buddy.PushProgress{Chunk: result.Chunks, PlainBytes: result.PlainBytes, SealedBytes: result.SealedBytes, Uploaded: result.Uploaded, Skipped: result.Skipped}
	job.FinishedAt = time.Now().UTC()
	job.mu.Unlock()
	s.afterBuddyJob(job)
}

// afterBuddyJob records schedule outcomes and notifies on both success and failure.
func (s *Server) afterBuddyJob(job *buddyJob) {
	snap := job.snapshot()
	if s.buddySchedules != nil && snap.ScheduleID != "" {
		s.buddySchedules.recordResult(snap.ScheduleID, snap.Receiver, snap.State == buddyJobSucceeded, snap.Error)
	}
	s.notifyBuddyJob(snap)
}

// notifyBuddyJob sends ntfy success/failure notes, gated on EnabledEvents.
func (s *Server) notifyBuddyJob(job buddyJobPublic) {
	if s.notifications == nil {
		return
	}
	settings := s.notifications.GetSettings()
	if !settings.Enabled {
		return
	}
	enabled := func(ev notifications.EventType) bool {
		for _, e := range settings.EnabledEvents {
			if e == ev {
				return true
			}
		}
		return false
	}
	switch job.State {
	case buddyJobSucceeded:
		if !enabled(notifications.EventBackupSuccess) {
			return
		}
		chunks, chain := 0, ""
		if job.Result != nil {
			chunks, chain = job.Result.Chunks, job.Result.Chain
		}
		_ = s.notifications.Send(notifications.Notification{
			Title:    fmt.Sprintf("Backup of %s to %s succeeded", job.Dataset, job.Receiver),
			Message:  fmt.Sprintf("source %s chain %s (%d chunks)", job.Source, chain, chunks),
			Severity: notifications.SeverityInfo,
			Tags:     []string{"white_check_mark", "naslos", "backup"},
			Time:     time.Now(),
		})
	case buddyJobFailed:
		if !enabled(notifications.EventBackupFailure) {
			return
		}
		_ = s.notifications.Send(notifications.Notification{
			Title:    fmt.Sprintf("Backup of %s to %s failed", job.Dataset, job.Receiver),
			Message:  fmt.Sprintf("source %s: %s", job.Source, job.Error),
			Severity: notifications.SeverityError,
			Tags:     []string{"x", "naslos", "backup"},
			Time:     time.Now(),
		})
	}
}

// handleBuddyJobs lists running plus recent finished jobs.
func (s *Server) handleBuddyJobs(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	jobs := s.ensureBuddyJobs().list()
	if jobs == nil {
		jobs = []buddyJobPublic{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"jobs": jobs})
}

// handleBuddyJobDetail reports or cancels one job: GET (detail incl. progress),
// DELETE (cancel a running send; the resume state stays so a retry continues).
func (s *Server) handleBuddyJobDetail(w http.ResponseWriter, req *http.Request) {
	id := strings.TrimPrefix(req.URL.Path, "/api/buddy/jobs/")
	if id == "" || strings.Contains(id, "/") {
		writeError(w, http.StatusBadRequest, "job id is required")
		return
	}
	job := s.ensureBuddyJobs().get(id)
	if job == nil {
		writeError(w, http.StatusNotFound, fmt.Sprintf("job %q not found", id))
		return
	}
	switch req.Method {
	case http.MethodGet:
		writeJSON(w, http.StatusOK, job.snapshot())
	case http.MethodDelete:
		job.mu.Lock()
		running := job.State == buddyJobRunning
		cancel := job.cancel
		job.mu.Unlock()
		if !running {
			writeError(w, http.StatusConflict, fmt.Sprintf("job %s is already %s", id, job.snapshot().State))
			return
		}
		cancel()
		writeJSON(w, http.StatusOK, map[string]string{"status": "cancelling", "id": id})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// waitBuddyJob is a test helper: it waits for a job to finish.
func (s *Server) waitBuddyJob(id string, timeout time.Duration) *buddyJobPublic {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		job := s.ensureBuddyJobs().get(id)
		if job == nil {
			return nil
		}
		snap := job.snapshot()
		if snap.State != buddyJobRunning {
			cp := snap
			return &cp
		}
		time.Sleep(20 * time.Millisecond)
	}
	return nil
}
