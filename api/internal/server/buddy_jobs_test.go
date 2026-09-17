package server

import (
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
)

// The job endpoints are verified through the same harness: a real receiver and
// a fake agent, with a stalled send stream when a job must stay running.

func TestBuddyJobDetailTracksProgress(t *testing.T) {
	harness := newSenderHarness(t, []byte("payload"))
	jobID := startSend(t, harness, map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/detail",
		"receiver": harness.receiverURL,
	})

	// The detail endpoint reports the job while it runs or after it finishes.
	rec := harness.call(t, http.MethodGet, "/api/buddy/jobs/"+jobID, nil)
	if rec.Code != http.StatusOK {
		t.Fatalf("job detail = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	var detail buddyJobPublic
	decode(t, rec, &detail)
	if detail.ID != jobID || (detail.State != buddyJobRunning && detail.State != buddyJobSucceeded) {
		t.Errorf("job detail = %+v, want job %s running or succeeded", detail, jobID)
	}

	job := waitSend(t, harness, jobID)
	if job.State != buddyJobSucceeded || job.Result == nil {
		t.Fatalf("job = %s (%s), want succeeded", job.State, job.Error)
	}

	rec = harness.call(t, http.MethodGet, "/api/buddy/jobs/"+jobID, nil)
	var finished buddyJobPublic
	decode(t, rec, &finished)
	if finished.State != buddyJobSucceeded || finished.Result == nil || finished.Result.Chain == "" {
		t.Errorf("finished job = %+v, want succeeded with a result", finished)
	}

	rec = harness.call(t, http.MethodGet, "/api/buddy/jobs", nil)
	var list struct {
		Jobs []buddyJobPublic `json:"jobs"`
	}
	decode(t, rec, &list)
	found := false
	for _, j := range list.Jobs {
		if j.ID == jobID {
			found = true
		}
	}
	if !found {
		t.Errorf("job %s missing from the job list (%d jobs)", jobID, len(list.Jobs))
	}

	// Unknown ids are 404, not 500.
	rec = harness.call(t, http.MethodGet, "/api/buddy/jobs/nope", nil)
	if rec.Code != http.StatusNotFound {
		t.Errorf("unknown job = %d, want 404", rec.Code)
	}
}

func TestBuddySendConflictsWhileRunning(t *testing.T) {
	payload := make([]byte, 3<<20)
	for i := range payload {
		payload[i] = byte(i)
	}
	harness := newSenderHarness(t, payload)
	harness.agent.mu.Lock()
	harness.agent.sendDelay = 3 * time.Second
	harness.agent.mu.Unlock()

	jobID := startSend(t, harness, map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/busy",
		"receiver": harness.receiverURL,
	})

	// Same (receiver, source) while running: refused.
	rec := harness.call(t, http.MethodPost, "/api/buddy/send", map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/busy",
		"receiver": harness.receiverURL,
	})
	if rec.Code != http.StatusConflict {
		t.Errorf("duplicate send = %d, want 409 (%s)", rec.Code, rec.Body.String())
	}
	if !strings.Contains(rec.Body.String(), "already running") {
		t.Errorf("conflict does not say a backup is already running: %s", rec.Body.String())
	}

	// The same dataset under another source is allowed while one runs: each job
	// snapshots under its own name, and a fan-out sends the same dataset to
	// several buddies at once (FR-BUD-15). Only the (receiver, source) pair is
	// exclusive, because that pair shares one resume-state file.
	rec = harness.call(t, http.MethodPost, "/api/buddy/send", map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/other",
		"receiver": harness.receiverURL,
	})
	if rec.Code != http.StatusAccepted {
		t.Errorf("same-dataset send = %d, want 202 (%s)", rec.Code, rec.Body.String())
	}

	job := waitSend(t, harness, jobID)
	if job.State != buddyJobSucceeded {
		t.Fatalf("first job = %s (%s), want succeeded", job.State, job.Error)
	}

	// Once finished the same source may run again.
	jobID2 := startSend(t, harness, map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/busy",
		"receiver": harness.receiverURL,
	})
	job2 := waitSend(t, harness, jobID2)
	if job2.State != buddyJobSucceeded {
		t.Errorf("second job = %s (%s), want succeeded", job2.State, job2.Error)
	}
}

func TestBuddySendCancelKeepsResumeState(t *testing.T) {
	payload := make([]byte, 3<<20)
	for i := range payload {
		payload[i] = byte(i)
	}
	harness := newSenderHarness(t, payload)
	harness.agent.mu.Lock()
	harness.agent.sendDelay = 3 * time.Second
	harness.agent.mu.Unlock()

	jobID := startSend(t, harness, map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/cancel",
		"receiver": harness.receiverURL,
	})

	// Wait until the send stream started (the resume state is written before
	// the first chunk leaves), then cancel mid-send: the delay holds the job
	// in running state.
	statePath := filepath.Join(sendStateDir(), sendStateName(harness.receiverURL, "naslos-test/cancel"))
	deadline := time.Now().Add(10 * time.Second)
	for {
		if _, err := os.Stat(statePath); err == nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("resume state was not written before the send stream started")
		}
		time.Sleep(20 * time.Millisecond)
	}

	// Cancel mid-send.
	rec := harness.call(t, http.MethodDelete, "/api/buddy/jobs/"+jobID, nil)
	if rec.Code != http.StatusOK {
		t.Fatalf("cancel = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	job := waitSend(t, harness, jobID)
	if job.State != buddyJobCancelled {
		t.Fatalf("job = %s (%s), want cancelled", job.State, job.Error)
	}

	// The resume state stays, so a retry continues the same chain.
	if _, err := os.Stat(statePath); err != nil {
		t.Fatalf("resume state missing after cancel: %v", err)
	}

	harness.agent.mu.Lock()
	harness.agent.sendDelay = 0
	harness.agent.mu.Unlock()

	retry := sendAndWait(t, harness, "test/data", "naslos-test/cancel")
	if !retry.Resumed {
		t.Error("the retry after a cancel did not report itself as a resume")
	}

	// Cancelling a finished job is a conflict, not a cancel.
	rec = harness.call(t, http.MethodDelete, "/api/buddy/jobs/"+jobID, nil)
	if rec.Code != http.StatusConflict {
		t.Errorf("cancel of a finished job = %d, want 409", rec.Code)
	}

	// The cancelled chain never published, the retry did: exactly one chain.
	client := buddy.NewClient(harness.receiverURL, harness.identity)
	chains, err := client.Chains("naslos-test/cancel")
	if err != nil {
		t.Fatalf("listing chains: %v", err)
	}
	if len(chains) != 1 || chains[0].Chain != retry.Chain {
		t.Errorf("chains = %+v, want exactly the retried chain %s", chains, retry.Chain)
	}
}
