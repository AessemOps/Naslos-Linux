package server

import (
	"net/http"
	"strings"
	"testing"
	"time"
)

// The app-job endpoints are verified through the standard auth-armed harness
// (adminRequest). Install jobs fail fast because the test catalog is empty and
// there is no cluster; the uninstall path uses a seeded orphaned record, which
// skips Helm and so can actually succeed.

// TestAppJobsRouteIsNotAnAppName pins the route-order gotcha: `/api/apps/jobs`
// must be the job list, never an app named "jobs".
func TestAppJobsRouteIsNotAnAppName(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodGet, "/api/apps/jobs", "")
	if rec.Code != http.StatusOK {
		t.Fatalf("GET /api/apps/jobs = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	var list struct {
		Jobs []appJobPublic `json:"jobs"`
	}
	decode(t, rec, &list)
	if list.Jobs == nil {
		t.Fatalf("jobs list = null, want an array")
	}

	rec = adminRequest(t, s, http.MethodGet, "/api/apps/jobs/nope", "")
	if rec.Code != http.StatusNotFound {
		t.Fatalf("GET /api/apps/jobs/nope = %d, want 404", rec.Code)
	}
}

// TestAppInstallJobEnqueuesAndReportsFailure covers the async install contract:
// POST answers 202 with a job handle, the job is observable, and it reports the
// catalog error instead of blocking the request.
func TestAppInstallJobEnqueuesAndReportsFailure(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodPost, "/api/apps", `{"name":"demo","values":{},"confirmed":true}`)
	if rec.Code != http.StatusAccepted {
		t.Fatalf("install = %d, want 202 (%s)", rec.Code, rec.Body.String())
	}
	var started struct {
		JobID string `json:"jobId"`
		State string `json:"state"`
	}
	decode(t, rec, &started)
	if started.JobID == "" || started.State != "running" {
		t.Fatalf("enqueue = %+v, want a running jobId", started)
	}

	job := s.waitAppJob(started.JobID, 10*time.Second)
	if job == nil {
		t.Fatalf("job %s never finished", started.JobID)
	}
	if job.State != appJobFailed {
		t.Fatalf("job state = %s (%s), want failed", job.State, job.Error)
	}
	if job.Error == "" {
		t.Fatal("failed job recorded no error")
	}
	if job.Kind != appJobInstall || job.App != "demo" {
		t.Fatalf("job = %+v, want install of demo", job)
	}
}

// TestAppJobConflictRejected pins the 409: a second job for an app with a job
// already running is refused.
func TestAppJobConflictRejected(t *testing.T) {
	s := newTestServer(t)
	s.ensureAppJobs().add(&appJob{
		appJobPublic: appJobPublic{
			ID:        "running-job",
			Kind:      appJobInstall,
			App:       "demo",
			State:     appJobRunning,
			Stage:     appJobInstalling,
			StartedAt: time.Now().UTC(),
		},
	})

	rec := adminRequest(t, s, http.MethodPost, "/api/apps", `{"name":"demo","values":{},"confirmed":true}`)
	if rec.Code != http.StatusConflict {
		t.Fatalf("conflicting install = %d, want 409 (%s)", rec.Code, rec.Body.String())
	}
	if !strings.Contains(rec.Body.String(), "already running") {
		t.Fatalf("conflict does not mention a running job: %s", rec.Body.String())
	}

	// A different app is not blocked by the running job.
	rec = adminRequest(t, s, http.MethodPost, "/api/apps", `{"name":"other","values":{},"confirmed":true}`)
	if rec.Code != http.StatusAccepted {
		t.Fatalf("other app install = %d, want 202 (%s)", rec.Code, rec.Body.String())
	}
}

// TestAppUninstallJobSucceeds exercises the success path end to end: an orphaned
// record needs no Helm call, so the job reaches `succeeded` and removes it.
func TestAppUninstallJobSucceeds(t *testing.T) {
	s := newSeededTestServer(t, `[{"name":"legacy","orphaned":true,"values":{},"exposure":{}}]`, "")

	rec := adminRequest(t, s, http.MethodDelete, "/api/apps/legacy", "")
	if rec.Code != http.StatusAccepted {
		t.Fatalf("uninstall = %d, want 202 (%s)", rec.Code, rec.Body.String())
	}
	var started struct {
		JobID string `json:"jobId"`
	}
	decode(t, rec, &started)

	job := s.waitAppJob(started.JobID, 10*time.Second)
	if job == nil || job.State != appJobSucceeded {
		t.Fatalf("uninstall job = %+v, want succeeded", job)
	}
	if job.Stage != appJobFinalizing {
		t.Fatalf("uninstall job stage = %s, want %s", job.Stage, appJobFinalizing)
	}

	rec = adminRequest(t, s, http.MethodGet, "/api/apps/legacy", "")
	if rec.Code != http.StatusNotFound {
		t.Fatalf("record after uninstall = %d, want 404", rec.Code)
	}
}

// TestAppUninstallUnknownAppIs404 keeps the synchronous not-found contract.
func TestAppUninstallUnknownAppIs404(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodDelete, "/api/apps/does-not-exist", "")
	if rec.Code != http.StatusNotFound {
		t.Fatalf("uninstall of an unknown app = %d, want 404 (%s)", rec.Code, rec.Body.String())
	}
}
