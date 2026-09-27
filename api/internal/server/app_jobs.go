package server

import (
	"context"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/apps"
)

// Async app lifecycle jobs (FR-APP-18).
//
// A cold install blocks on Helm's `--wait` for up to five minutes and used to
// run inside the request: the handler passed r.Context() straight through, so a
// closed modal or a navigated-away page killed the install with it. Jobs
// decouple the two: the handler validates, enqueues and answers 202, while the
// install runs under a server-owned context the operator polls.
//
// Jobs live in memory; the app record remains the durable outcome. A job
// interrupted by an API restart simply stops: reconcileApps re-converges the
// records and routes on boot, and GET /api/apps shows the coarse Helm status.

// appJobState is the lifecycle of one app job.
type appJobState string

const (
	appJobRunning   appJobState = "running"
	appJobSucceeded appJobState = "succeeded"
	appJobFailed    appJobState = "failed"
)

// appJobKind is which lifecycle operation the job runs.
type appJobKind string

const (
	appJobInstall   appJobKind = "install"
	appJobUpgrade   appJobKind = "upgrade"
	appJobUninstall appJobKind = "uninstall"
)

// appJobStage is the coarse install phase (Helm's `--wait` gives no percentage).
type appJobStage string

const (
	appJobPreparing  appJobStage = "preparing"
	appJobInstalling appJobStage = "installing"
	appJobFinalizing appJobStage = "finalizing"
)

// appJobPublic is the serializable snapshot of one job: everything the API
// reports, without the mutex, context or request inputs.
type appJobPublic struct {
	ID         string      `json:"id"`
	Kind       appJobKind  `json:"kind"`
	App        string      `json:"app"`
	State      appJobState `json:"state"`
	Stage      appJobStage `json:"stage"`
	Message    string      `json:"message,omitempty"`
	StartedAt  time.Time   `json:"startedAt"`
	FinishedAt time.Time   `json:"finishedAt,omitempty"`
	BaseDomain string      `json:"baseDomain,omitempty"`
	Error      string      `json:"error,omitempty"`
}

// appJob is one async lifecycle operation.
type appJob struct {
	appJobPublic

	// request is the validated install input (kind == install).
	request apps.InstallRequest
	// values is the reconfigure overlay (kind == upgrade).
	values map[string]interface{}
	// result is the resulting view of a successful install/upgrade; kept for
	// internal use (the drawer links to the Installed tab, not this payload).
	result *apps.View

	mu     sync.Mutex
	ctx    context.Context
	cancel context.CancelFunc
	done   chan struct{}
}

// snapshot returns a copy safe to serialize.
func (j *appJob) snapshot() appJobPublic {
	j.mu.Lock()
	defer j.mu.Unlock()
	return j.appJobPublic
}

// appJobManager keeps running jobs plus the last ~20 finished ones.
type appJobManager struct {
	mu    sync.Mutex
	jobs  map[string]*appJob
	order []string
}

func newAppJobManager() *appJobManager {
	return &appJobManager{jobs: make(map[string]*appJob)}
}

// conflicting returns a running job for an app, whatever its kind: Helm
// serialises operations on a release, so a second job for the same app would
// race the first.
func (m *appJobManager) conflicting(app string) *appJob {
	m.mu.Lock()
	defer m.mu.Unlock()
	for _, job := range m.jobs {
		job.mu.Lock()
		running := job.State == appJobRunning
		sameApp := job.App == app
		job.mu.Unlock()
		if running && sameApp {
			return job
		}
	}
	return nil
}

func (m *appJobManager) add(job *appJob) {
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
		if state == appJobRunning || finished < 20 {
			kept = append(kept, m.order[i])
			if state != appJobRunning {
				finished++
			}
		}
	}
	for i, j := 0, len(kept)-1; i < j; i, j = i+1, j-1 {
		kept[i], kept[j] = kept[j], kept[i]
	}
	for id := range m.jobs {
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
	}
	m.order = kept
}

func (m *appJobManager) get(id string) *appJob {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.jobs[id]
}

func (m *appJobManager) list() []appJobPublic {
	m.mu.Lock()
	ids := append([]string(nil), m.order...)
	m.mu.Unlock()
	out := make([]appJobPublic, 0, len(ids))
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

// cancelAll cancels every running job (server shutdown).
func (m *appJobManager) cancelAll() {
	m.mu.Lock()
	jobs := make([]*appJob, 0, len(m.jobs))
	for _, j := range m.jobs {
		jobs = append(jobs, j)
	}
	m.mu.Unlock()
	for _, j := range jobs {
		j.mu.Lock()
		running := j.State == appJobRunning
		cancel := j.cancel
		j.mu.Unlock()
		if running && cancel != nil {
			cancel()
		}
	}
}

// writeAppJobConflict answers 409 for a second job on an app that already has
// one running, naming the running job.
func writeAppJobConflict(w http.ResponseWriter, job *appJob) {
	c := job.snapshot()
	writeError(w, http.StatusConflict, fmt.Sprintf(
		"an app job for %q is already running (job %s, %s, started %s)",
		c.App, c.ID, c.Kind, c.StartedAt.Format(time.RFC3339)))
}

// ensureAppJobs lazily creates the manager (test harnesses build Server by hand).
func (s *Server) ensureAppJobs() *appJobManager {
	if s.appJobs == nil {
		s.appJobs = newAppJobManager()
	}
	return s.appJobs
}

// enqueueAppJob registers and starts a lifecycle job. The caller must have
// checked for a conflict first.
func (s *Server) enqueueAppJob(kind appJobKind, app, baseDomain string, req apps.InstallRequest, values map[string]interface{}) *appJob {
	ctx, cancel := context.WithCancel(context.Background())
	job := &appJob{
		appJobPublic: appJobPublic{
			ID:         randomJobID(),
			Kind:       kind,
			App:        app,
			State:      appJobRunning,
			Stage:      appJobPreparing,
			StartedAt:  time.Now().UTC(),
			BaseDomain: baseDomain,
		},
		request: req,
		values:  values,
		ctx:     ctx,
		cancel:  cancel,
		done:    make(chan struct{}),
	}
	s.ensureAppJobs().add(job)
	go s.runAppJob(job)
	return job
}

// runAppJob executes the lifecycle operation the handler used to run inline. The
// context is the job's own: a disconnected operator no longer kills the job.
func (s *Server) runAppJob(job *appJob) {
	defer close(job.done)

	progress := func(stage, message string) {
		job.mu.Lock()
		job.Stage = appJobStage(stage)
		job.Message = message
		job.mu.Unlock()
	}
	finish := func(state appJobState, errMsg string, view *apps.View) {
		job.mu.Lock()
		job.State = state
		job.Error = errMsg
		job.FinishedAt = time.Now().UTC()
		job.result = view
		job.mu.Unlock()
	}

	job.mu.Lock()
	kind, app := job.Kind, job.App
	req, values := job.request, job.values
	job.mu.Unlock()

	if s.appManager == nil {
		finish(appJobFailed, "app management is not available", nil)
		return
	}

	ctx := job.ctx
	var view *apps.View
	var err error
	switch kind {
	case appJobInstall:
		view, err = s.appManager.InstallWithProgress(ctx, req, progress)
	case appJobUpgrade:
		view, err = s.appManager.UpgradeWithProgress(ctx, app, values, progress)
	case appJobUninstall:
		err = s.appManager.UninstallWithProgress(ctx, app, progress)
	default:
		err = fmt.Errorf("unknown app job kind %q", kind)
	}
	if err != nil {
		finish(appJobFailed, err.Error(), nil)
		return
	}
	finish(appJobSucceeded, "", view)
}

// handleAppJobs lists running plus recent finished app jobs.
func (s *Server) handleAppJobs(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	jobs := s.ensureAppJobs().list()
	if jobs == nil {
		jobs = []appJobPublic{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"jobs": jobs})
}

// handleAppJobDetail reports one job (the poll target).
func (s *Server) handleAppJobDetail(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	id := strings.TrimPrefix(req.URL.Path, "/api/apps/jobs/")
	if id == "" || strings.Contains(id, "/") {
		writeError(w, http.StatusBadRequest, "job id is required")
		return
	}
	job := s.ensureAppJobs().get(id)
	if job == nil {
		writeError(w, http.StatusNotFound, fmt.Sprintf("job %q not found", id))
		return
	}
	writeJSON(w, http.StatusOK, job.snapshot())
}

// waitAppJob is a test helper: it waits for a job to finish.
func (s *Server) waitAppJob(id string, timeout time.Duration) *appJobPublic {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		job := s.ensureAppJobs().get(id)
		if job == nil {
			return nil
		}
		snap := job.snapshot()
		if snap.State != appJobRunning {
			cp := snap
			return &cp
		}
		time.Sleep(20 * time.Millisecond)
	}
	return nil
}
