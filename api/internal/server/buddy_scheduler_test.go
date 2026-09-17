package server

import (
	"bytes"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
	"github.com/AessemOps/Naslos-Linux/api/internal/notifications"
)

// Scheduler tests reuse the sender harness: a real receiver plus a fake agent,
// with the schedule store pointed at a temp file.

// ntfyCapture records the notification posts a test triggers.
type ntfyCapture struct {
	server *httptest.Server
	mu     sync.Mutex
	posts  int
	titles []string
}

func newNtfyCapture(t *testing.T) *ntfyCapture {
	t.Helper()
	c := &ntfyCapture{}
	c.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		c.mu.Lock()
		c.posts++
		c.titles = append(c.titles, r.Header.Get("Title"))
		c.mu.Unlock()
		w.WriteHeader(http.StatusOK)
	}))
	t.Cleanup(c.server.Close)
	return c
}

func (c *ntfyCapture) count() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.posts
}

// enableBackupNotifications wires a notification manager that posts both backup
// outcomes to the capture server.
func enableBackupNotifications(t *testing.T, h *senderHarness, c *ntfyCapture) {
	t.Helper()
	mgr := notifications.NewManager(filepath.Join(t.TempDir(), "notifications.json"))
	if err := mgr.UpdateSettings(notifications.Settings{
		Enabled:       true,
		ServerURL:     c.server.URL,
		Topic:         "naslos-test",
		EnabledEvents: []notifications.EventType{notifications.EventBackupFailure, notifications.EventBackupSuccess},
		MinSeverity:   notifications.SeverityInfo,
	}); err != nil {
		t.Fatalf("enabling notifications: %v", err)
	}
	h.server.notifications = mgr
}

// useTempSchedules points the harness at an isolated schedule store.
func useTempSchedules(t *testing.T, h *senderHarness) *buddyScheduleStore {
	t.Helper()
	store := newBuddyScheduleStore(filepath.Join(t.TempDir(), "schedules.json"))
	if err := store.load(); err != nil {
		t.Fatalf("loading schedule store: %v", err)
	}
	h.server.buddySchedules = store
	h.server.ensureBuddyJobs()
	return store
}

// createSchedule creates a schedule over the API and returns its id.
func createSchedule(t *testing.T, h *senderHarness, body map[string]any) string {
	t.Helper()
	rec := h.call(t, http.MethodPost, "/api/buddy/schedules", body)
	if rec.Code != http.StatusOK {
		t.Fatalf("create schedule = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	var created struct {
		Schedule buddyScheduleEntry `json:"schedule"`
	}
	decode(t, rec, &created)
	if created.Schedule.ID == "" || created.Schedule.NextRun.IsZero() {
		t.Fatalf("schedule = %+v, want an id and a next run", created.Schedule)
	}
	return created.Schedule.ID
}

// forceDue moves a schedule's next run into the past.
func forceDue(t *testing.T, store *buddyScheduleStore, id string) {
	t.Helper()
	entry := store.get(id)
	if entry == nil {
		t.Fatalf("schedule %s not found", id)
	}
	entry.NextRun = time.Now().UTC().Add(-time.Minute)
	if err := store.put(entry); err != nil {
		t.Fatalf("forcing schedule due: %v", err)
	}
}

// waitScheduleJob waits for the job a schedule started.
func waitScheduleJob(t *testing.T, h *senderHarness, scheduleID string) buddyJobPublic {
	t.Helper()
	deadline := time.Now().Add(60 * time.Second)
	for time.Now().Before(deadline) {
		for _, j := range h.server.ensureBuddyJobs().list() {
			if j.ScheduleID == scheduleID && j.State != buddyJobRunning {
				return j
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("no finished job for schedule %s", scheduleID)
	return buddyJobPublic{}
}

func countScheduleJobs(h *senderHarness, scheduleID string) int {
	n := 0
	for _, j := range h.server.ensureBuddyJobs().list() {
		if j.ScheduleID == scheduleID {
			n++
		}
	}
	return n
}

func TestBuddySchedulesCRUD(t *testing.T) {
	harness := newSenderHarness(t, []byte("payload"))
	useTempSchedules(t, harness)

	id := createSchedule(t, harness, map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/sched",
		"receiver": harness.receiverURL,
		"cadence":  "daily",
		"runAt":    "02:30",
	})

	rec := harness.call(t, http.MethodGet, "/api/buddy/schedules", nil)
	var list struct {
		Schedules []*buddyScheduleEntry `json:"schedules"`
	}
	decode(t, rec, &list)
	if len(list.Schedules) != 1 || list.Schedules[0].ID != id {
		t.Fatalf("schedules = %+v, want the one created", list.Schedules)
	}

	// Update: disable it.
	rec = harness.call(t, http.MethodPost, "/api/buddy/schedules", map[string]any{
		"id":       id,
		"dataset":  "test/data",
		"source":   "naslos-test/sched",
		"receiver": harness.receiverURL,
		"cadence":  "daily",
		"runAt":    "02:30",
		"enabled":  false,
	})
	if rec.Code != http.StatusOK {
		t.Fatalf("update = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}

	// Validation: bad cadence, bad time, traversal source, bad URL.
	for _, body := range []map[string]any{
		{"dataset": "test/data", "source": "a/b", "receiver": harness.receiverURL, "cadence": "minutely", "runAt": ""},
		{"dataset": "test/data", "source": "a/b", "receiver": harness.receiverURL, "cadence": "daily", "runAt": "25:00"},
		{"dataset": "test/data", "source": "../etc", "receiver": harness.receiverURL, "cadence": "hourly"},
		{"dataset": "test/data", "source": "a/b", "receiver": "ftp://buddy", "cadence": "hourly"},
		{"dataset": "test/data", "source": "a/b", "receiver": harness.receiverURL, "cadence": "weekly", "runAt": "02:30"},
	} {
		rec := harness.call(t, http.MethodPost, "/api/buddy/schedules", body)
		if rec.Code != http.StatusBadRequest {
			t.Errorf("invalid schedule %v = %d, want 400 (%s)", body, rec.Code, rec.Body.String())
		}
	}

	rec = harness.call(t, http.MethodDelete, "/api/buddy/schedules?id="+id, nil)
	if rec.Code != http.StatusOK {
		t.Fatalf("delete = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
	rec = harness.call(t, http.MethodDelete, "/api/buddy/schedules?id="+id, nil)
	if rec.Code != http.StatusNotFound {
		t.Errorf("second delete = %d, want 404", rec.Code)
	}
}

func TestBuddySchedulerFiresDueEntryAndPrunes(t *testing.T) {
	harness := newSenderHarness(t, []byte("payload"))
	store := useTempSchedules(t, harness)

	// A first chain, so the scheduled run can prune back to one. This manual
	// send runs before notifications are wired, so it stays silent.
	sendAndWait(t, harness, "test/data", "naslos-test/sched-prune")

	ntfy := newNtfyCapture(t)
	enableBackupNotifications(t, harness, ntfy)

	id := createSchedule(t, harness, map[string]any{
		"dataset":   "test/data",
		"source":    "naslos-test/sched-prune",
		"receiver":  harness.receiverURL,
		"cadence":   "hourly",
		"pruneKeep": 1,
	})
	forceDue(t, store, id)

	harness.server.runDueSchedules(time.Now().UTC())
	job := waitScheduleJob(t, harness, id)
	if job.State != buddyJobSucceeded || job.Result == nil {
		t.Fatalf("scheduled job = %s (%s), want succeeded", job.State, job.Error)
	}

	entry := store.get(id)
	if entry.LastResult != "ok" || entry.LastRun.IsZero() || !entry.NextRun.After(time.Now().UTC()) {
		t.Errorf("schedule after run = %+v, want ok + advanced next run", entry)
	}

	client := buddy.NewClient(harness.receiverURL, harness.identity)
	chains, err := client.Chains("naslos-test/sched-prune")
	if err != nil {
		t.Fatalf("listing chains: %v", err)
	}
	// pruneKeep=1 cannot leave exactly one chain here: the scheduled run is
	// incremental off the manual one, so dropping the base would leave a backup
	// that only fails later at verify/restore time. Retention keeps the whole
	// sequence instead (FR-BUD-09).
	if len(chains) != 2 {
		t.Errorf("chains = %d, want the incremental plus the base it descends from", len(chains))
	}
	var restored bytes.Buffer
	if _, err := client.Restore(buddy.RestoreOptions{Source: "naslos-test/sched-prune", Out: &restored}); err != nil {
		t.Errorf("restore after the scheduled prune: %v", err)
	}
	if restored.Len() == 0 {
		t.Error("the restored stream is empty")
	}

	if ntfy.count() != 1 {
		t.Errorf("ntfy posts = %d, want the success note", ntfy.count())
	}
}

func TestBuddySchedulerFailureNotifies(t *testing.T) {
	harness := newSenderHarness(t, []byte("payload"))
	store := useTempSchedules(t, harness)
	ntfy := newNtfyCapture(t)
	enableBackupNotifications(t, harness, ntfy)

	dead := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = w.Write([]byte(`{"error":"boom"}`))
	}))
	t.Cleanup(dead.Close)

	id := createSchedule(t, harness, map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/sched-dead",
		"receiver": dead.URL,
		"cadence":  "hourly",
	})
	forceDue(t, store, id)

	harness.server.runDueSchedules(time.Now().UTC())
	job := waitScheduleJob(t, harness, id)
	if job.State != buddyJobFailed {
		t.Fatalf("scheduled job = %s, want failed against a dead receiver", job.State)
	}
	entry := store.get(id)
	if entry.LastResult != "failed" || entry.LastError == "" {
		t.Errorf("schedule after failure = %+v, want failed + error", entry)
	}
	if ntfy.count() != 1 {
		t.Errorf("ntfy posts = %d, want the failure note", ntfy.count())
	}
}

func TestBuddySchedulerCatchUpFiresOnceAndDisabledNeverFires(t *testing.T) {
	payload := make([]byte, 2<<20)
	for i := range payload {
		payload[i] = byte(i)
	}
	harness := newSenderHarness(t, payload)
	store := useTempSchedules(t, harness)
	harness.agent.mu.Lock()
	harness.agent.sendDelay = 3 * time.Second
	harness.agent.mu.Unlock()

	due := createSchedule(t, harness, map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/catchup",
		"receiver": harness.receiverURL,
		"cadence":  "hourly",
	})
	forceDue(t, store, due)

	// Catch-up: the first tick starts it; the second tick (while it runs) must
	// not start a second job for the same entry.
	harness.server.runDueSchedules(time.Now().UTC())
	harness.server.runDueSchedules(time.Now().UTC())
	// Let the runner tick once more mid-flight through the public path.
	harness.server.runDueSchedules(time.Now().UTC())
	if n := countScheduleJobs(harness, due); n != 1 {
		t.Fatalf("jobs for the due entry = %d, want exactly 1", n)
	}

	disabled := createSchedule(t, harness, map[string]any{
		"dataset":  "test/data",
		"source":   "naslos-test/disabled",
		"receiver": harness.receiverURL,
		"cadence":  "hourly",
		"enabled":  false,
	})
	forceDue(t, store, disabled)
	harness.server.runDueSchedules(time.Now().UTC())
	if n := countScheduleJobs(harness, disabled); n != 0 {
		t.Fatalf("jobs for a disabled entry = %d, want 0", n)
	}

	job := waitScheduleJob(t, harness, due)
	if job.State != buddyJobSucceeded {
		t.Errorf("catch-up job = %s (%s), want succeeded", job.State, job.Error)
	}
}

func TestComputeNextRun(t *testing.T) {
	now := time.Date(2026, 9, 14, 10, 15, 0, 0, time.UTC) // a Monday
	hourly := &buddyScheduleEntry{Cadence: buddyCadenceHourly}
	if next := computeNextRun(hourly, now); !next.Equal(time.Date(2026, 9, 14, 11, 0, 0, 0, time.UTC)) {
		t.Errorf("hourly next = %s, want 11:00", next)
	}
	daily := &buddyScheduleEntry{Cadence: buddyCadenceDaily, RunAt: "02:30"}
	if next := computeNextRun(daily, now); !next.Equal(time.Date(2026, 9, 15, 2, 30, 0, 0, time.UTC)) {
		t.Errorf("daily next = %s, want tomorrow 02:30", next)
	}
	weekly := &buddyScheduleEntry{Cadence: buddyCadenceWeekly, RunAt: "Mon 09:00"}
	// Monday 10:15 is past Monday 09:00, so the next run is next Monday.
	if next := computeNextRun(weekly, now); !next.Equal(time.Date(2026, 9, 21, 9, 0, 0, 0, time.UTC)) {
		t.Errorf("weekly next = %s, want next Monday 09:00", next)
	}
	future := &buddyScheduleEntry{Cadence: buddyCadenceWeekly, RunAt: "Mon 12:00"}
	if next := computeNextRun(future, now); !next.Equal(time.Date(2026, 9, 14, 12, 0, 0, 0, time.UTC)) {
		t.Errorf("weekly next = %s, want today 12:00", next)
	}
}

func TestScheduleStoreRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "schedules.json")
	store := newBuddyScheduleStore(path)
	entry := &buddyScheduleEntry{
		ID: randomJobID(), Dataset: "test/data", Source: "a/b",
		Receiver: "http://buddy", Cadence: buddyCadenceDaily,
		RunAt: "02:30", Enabled: true,
		NextRun: time.Date(2026, 9, 15, 2, 30, 0, 0, time.UTC),
	}
	if err := store.put(entry); err != nil {
		t.Fatalf("put: %v", err)
	}
	loaded := newBuddyScheduleStore(path)
	if err := loaded.load(); err != nil {
		t.Fatalf("load: %v", err)
	}
	got := loaded.get(entry.ID)
	if got == nil || got.Source != "a/b" || !got.NextRun.Equal(entry.NextRun) {
		t.Fatalf("round trip = %+v, want the stored entry", got)
	}
	if !strings.HasPrefix(got.Receiver, "http://") {
		t.Errorf("receiver = %q mangled by the round trip", got.Receiver)
	}
}
