package server

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
)

// Scheduled backups (FR-BUD-15).
//
// Entries persist as JSON beside the peer registry and are managed over the API.
// The runner ticks (default every minute) and starts a send job through the same
// path as a manual Back up now, so the (receiver, source) mutual exclusion in
// the job manager covers both. A run missed while the API pod was down fires
// once at startup (catch-up).

// buddyCadence is the scheduler interval.
type buddyCadence string

const (
	buddyCadenceHourly buddyCadence = "hourly"
	buddyCadenceDaily  buddyCadence = "daily"
	buddyCadenceWeekly buddyCadence = "weekly"
)

// buddyScheduleEntry is one scheduled backup.
type buddyScheduleEntry struct {
	ID        string       `json:"id"`
	Dataset   string       `json:"dataset"`
	Source    string       `json:"source"`
	Receiver  string       `json:"receiver"`
	Cadence   buddyCadence `json:"cadence"`
	RunAt     string       `json:"runAt,omitempty"`
	PruneKeep int          `json:"pruneKeep,omitempty"`
	Enabled   bool         `json:"enabled"`
	LastRun   time.Time    `json:"lastRun,omitempty"`
	// LastResult is "" (never), "ok" or "failed".
	LastResult string    `json:"lastResult,omitempty"`
	LastError  string    `json:"lastError,omitempty"`
	NextRun    time.Time `json:"nextRun"`
}

// buddyScheduleStore persists entries as JSON, modeled on buddy.NewPeerStore.
type buddyScheduleStore struct {
	path string

	mu      sync.RWMutex
	entries map[string]*buddyScheduleEntry
}

func newBuddyScheduleStore(path string) *buddyScheduleStore {
	return &buddyScheduleStore{path: path, entries: make(map[string]*buddyScheduleEntry)}
}

func buddySchedulesPath() string {
	if env := os.Getenv("BUDDY_SCHEDULES"); env != "" {
		return env
	}
	return "/var/lib/naslos/buddy-schedules.json"
}

func (s *buddyScheduleStore) load() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.entries = make(map[string]*buddyScheduleEntry)
	if s.path == "" {
		return nil
	}
	data, err := os.ReadFile(s.path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	var entries []*buddyScheduleEntry
	if err := json.Unmarshal(data, &entries); err != nil {
		return fmt.Errorf("parsing schedule store %s: %w", s.path, err)
	}
	for _, e := range entries {
		if e == nil || e.ID == "" {
			continue
		}
		s.entries[e.ID] = e
	}
	return nil
}

func (s *buddyScheduleStore) saveLocked() error {
	if s.path == "" {
		return nil
	}
	entries := make([]*buddyScheduleEntry, 0, len(s.entries))
	for _, e := range s.entries {
		entries = append(entries, e)
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i].ID < entries[j].ID })
	data, err := json.MarshalIndent(entries, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')
	if dir := filepath.Dir(s.path); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0755); err != nil {
			return err
		}
	}
	tmp, err := os.CreateTemp(filepath.Dir(s.path), ".schedules-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	if err := os.Chmod(tmpName, 0644); err != nil {
		return err
	}
	return os.Rename(tmpName, s.path)
}

func (s *buddyScheduleStore) list() []*buddyScheduleEntry {
	s.mu.RLock()
	defer s.mu.RUnlock()
	out := make([]*buddyScheduleEntry, 0, len(s.entries))
	for _, e := range s.entries {
		cp := *e
		out = append(out, &cp)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out
}

func (s *buddyScheduleStore) get(id string) *buddyScheduleEntry {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if e, ok := s.entries[id]; ok {
		cp := *e
		return &cp
	}
	return nil
}

func (s *buddyScheduleStore) put(e *buddyScheduleEntry) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	cp := *e
	s.entries[e.ID] = &cp
	return s.saveLocked()
}

func (s *buddyScheduleStore) remove(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.entries[id]; !ok {
		return fmt.Errorf("schedule %q not found", id)
	}
	delete(s.entries, id)
	return s.saveLocked()
}

// recordResult stores the outcome of a job started for a schedule and advances
// nextRun from completion time.
func (s *buddyScheduleStore) recordResult(id string, ok bool, jobErr string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, found := s.entries[id]
	if !found {
		return
	}
	now := time.Now().UTC()
	e.LastRun = now
	if ok {
		e.LastResult = "ok"
		e.LastError = ""
	} else {
		e.LastResult = "failed"
		e.LastError = jobErr
	}
	e.NextRun = computeNextRun(e, now)
	_ = s.saveLocked()
}

// parseRunAt parses "HH:MM" (daily) or "Mon 15:04" (weekly).
func parseRunAt(cadence buddyCadence, runAt string) (time.Weekday, int, int, error) {
	text := strings.TrimSpace(runAt)
	if cadence == buddyCadenceHourly {
		return time.Sunday, 0, 0, nil
	}
	if cadence == buddyCadenceDaily {
		var h, m int
		if _, err := fmt.Sscanf(text, "%d:%d", &h, &m); err != nil || h < 0 || h > 23 || m < 0 || m > 59 || len(text) != 5 {
			return time.Sunday, 0, 0, fmt.Errorf("runAt must be HH:MM in UTC (e.g. 02:30)")
		}
		return time.Sunday, h, m, nil
	}
	// Weekly: "Mon 02:30".
	parts := strings.Fields(text)
	if len(parts) != 2 {
		return time.Sunday, 0, 0, fmt.Errorf("runAt must be \"Mon 02:30\" in UTC for weekly schedules")
	}
	var weekday time.Weekday
	found := false
	for d := time.Sunday; d <= time.Saturday; d++ {
		if strings.EqualFold(d.String()[:3], parts[0]) {
			weekday, found = d, true
			break
		}
	}
	if !found {
		return time.Sunday, 0, 0, fmt.Errorf("runAt weekday must be one of Sun Mon Tue Wed Thu Fri Sat")
	}
	var h, m int
	if _, err := fmt.Sscanf(parts[1], "%d:%d", &h, &m); err != nil || h < 0 || h > 23 || m < 0 || m > 59 || len(parts[1]) != 5 {
		return time.Sunday, 0, 0, fmt.Errorf("runAt time must be HH:MM in UTC (e.g. Mon 02:30)")
	}
	return weekday, h, m, nil
}

// computeNextRun returns the next run strictly after now.
func computeNextRun(e *buddyScheduleEntry, now time.Time) time.Time {
	now = now.UTC()
	switch e.Cadence {
	case buddyCadenceHourly:
		return now.Truncate(time.Hour).Add(time.Hour)
	case buddyCadenceDaily:
		_, h, m, err := parseRunAt(e.Cadence, e.RunAt)
		if err != nil {
			return now.Add(24 * time.Hour)
		}
		next := time.Date(now.Year(), now.Month(), now.Day(), h, m, 0, 0, time.UTC)
		if !next.After(now) {
			next = next.Add(24 * time.Hour)
		}
		return next
	case buddyCadenceWeekly:
		weekday, h, m, err := parseRunAt(e.Cadence, e.RunAt)
		if err != nil {
			return now.Add(7 * 24 * time.Hour)
		}
		next := time.Date(now.Year(), now.Month(), now.Day(), h, m, 0, 0, time.UTC)
		for next.Weekday() != weekday || !next.After(now) {
			next = next.Add(24 * time.Hour)
		}
		return next
	default:
		return now.Add(time.Hour)
	}
}

// ensureBuddySchedules lazily creates the store (test harnesses build Server by hand).
func (s *Server) ensureBuddySchedules() *buddyScheduleStore {
	if s.buddySchedules == nil {
		s.buddySchedules = newBuddyScheduleStore(buddySchedulesPath())
		if err := s.buddySchedules.load(); err != nil {
			log.Printf("Warning: could not load the buddy schedule store: %v", err)
		}
	}
	return s.buddySchedules
}

// validateSchedule checks cadence/runAt, source, receiver and dataset shape. The
// agent-known-dataset check is best-effort: when the agent positively lists
// datasets and the name is absent it is refused; when the agent cannot be asked
// the send itself surfaces the problem.
func (s *Server) validateSchedule(e *buddyScheduleEntry) error {
	if strings.TrimSpace(e.Dataset) == "" {
		return fmt.Errorf("dataset is required")
	}
	if strings.Contains(e.Dataset, "@") || strings.HasPrefix(e.Dataset, "/") || strings.Contains(e.Dataset, "..") {
		return fmt.Errorf("invalid dataset %q", e.Dataset)
	}
	if err := buddy.ValidateSource(strings.TrimSpace(e.Source)); err != nil {
		return err
	}
	if _, err := normalizeReceiverURL(e.Receiver); err != nil {
		return err
	}
	switch e.Cadence {
	case buddyCadenceHourly, buddyCadenceDaily, buddyCadenceWeekly:
	default:
		return fmt.Errorf("cadence must be hourly, daily or weekly")
	}
	if _, _, _, err := parseRunAt(e.Cadence, e.RunAt); err != nil {
		return err
	}
	if e.PruneKeep < 0 {
		return fmt.Errorf("pruneKeep cannot be negative")
	}
	if s.agent != nil {
		if datasets, err := s.agent.ListDatasets(); err == nil {
			// Exact match only. A pool-prefix match would accept any sibling name
			// that happens to exist ("test/nope" while "test" is a pool), and the
			// schedule would then fail at run time instead of at creation time
			// (NAS-018).
			known := false
			for _, d := range datasets {
				if d.Name == e.Dataset {
					known = true
					break
				}
			}
			if !known {
				return fmt.Errorf("dataset %q is not known to the agent", e.Dataset)
			}
		}
	}
	return nil
}

// handleBuddySchedules manages scheduled backups:
//
//	GET    /api/buddy/schedules → {"schedules":[…]}
//	POST   /api/buddy/schedules {id?,dataset,source,receiver,cadence,runAt,pruneKeep,enabled}
//	DELETE /api/buddy/schedules?id=…
func (s *Server) handleBuddySchedules(w http.ResponseWriter, req *http.Request) {
	store := s.ensureBuddySchedules()
	switch req.Method {
	case http.MethodGet:
		schedules := store.list()
		if schedules == nil {
			schedules = []*buddyScheduleEntry{}
		}
		writeJSON(w, http.StatusOK, map[string]any{"schedules": schedules})

	case http.MethodPost:
		var request struct {
			ID        string       `json:"id"`
			Dataset   string       `json:"dataset"`
			Source    string       `json:"source"`
			Receiver  string       `json:"receiver"`
			Cadence   buddyCadence `json:"cadence"`
			RunAt     string       `json:"runAt"`
			PruneKeep int          `json:"pruneKeep"`
			Enabled   *bool        `json:"enabled"`
		}
		if err := json.NewDecoder(http.MaxBytesReader(w, req.Body, 64<<10)).Decode(&request); err != nil {
			writeError(w, http.StatusBadRequest, "invalid request: "+err.Error())
			return
		}
		entry := &buddyScheduleEntry{
			ID:        strings.TrimSpace(request.ID),
			Dataset:   strings.TrimSpace(request.Dataset),
			Source:    strings.TrimSpace(request.Source),
			Cadence:   request.Cadence,
			RunAt:     strings.TrimSpace(request.RunAt),
			PruneKeep: request.PruneKeep,
			Enabled:   true,
		}
		if receiver, err := normalizeReceiverURL(request.Receiver); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		} else {
			entry.Receiver = receiver
		}
		if request.Enabled != nil {
			entry.Enabled = *request.Enabled
		}
		if entry.ID == "" {
			entry.ID = randomJobID()
		} else if existing := store.get(entry.ID); existing != nil {
			entry.LastRun, entry.LastResult, entry.LastError = existing.LastRun, existing.LastResult, existing.LastError
		}
		if err := s.validateSchedule(entry); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		entry.NextRun = computeNextRun(entry, time.Now().UTC())
		if err := store.put(entry); err != nil {
			writeError(w, http.StatusInternalServerError, "saving the schedule: "+err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]any{"schedule": entry})

	case http.MethodDelete:
		id := strings.TrimSpace(req.URL.Query().Get("id"))
		if id == "" {
			writeError(w, http.StatusBadRequest, "id is required")
			return
		}
		if err := store.remove(id); err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "deleted", "id": id})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// schedulerInterval is a minute by default; BUDDY_SCHEDULER_INTERVAL_MS overrides
// it for tests.
func schedulerInterval() time.Duration {
	if v := os.Getenv("BUDDY_SCHEDULER_INTERVAL_MS"); v != "" {
		var ms int
		if _, err := fmt.Sscanf(v, "%d", &ms); err == nil && ms > 0 {
			return time.Duration(ms) * time.Millisecond
		}
	}
	return time.Minute
}

// startBuddyScheduler starts the runner; catch-up fires entries whose nextRun is
// already past exactly once.
func (s *Server) startBuddyScheduler() {
	if s.schedulerStop != nil {
		return
	}
	s.ensureBuddySchedules()
	s.ensureBuddyJobs()
	stop := make(chan struct{})
	s.schedulerStop = stop
	s.runDueSchedules(time.Now().UTC())
	go func() {
		ticker := time.NewTicker(schedulerInterval())
		defer ticker.Stop()
		for {
			select {
			case <-stop:
				return
			case now := <-ticker.C:
				s.runDueSchedules(now.UTC())
			}
		}
	}()
	log.Printf("Buddy scheduler started (interval %s)", schedulerInterval())
}

func (s *Server) stopBuddyScheduler() {
	if s.schedulerStop != nil {
		close(s.schedulerStop)
		s.schedulerStop = nil
	}
}

// runDueSchedules starts one job per due enabled entry. A conflicting manual or
// scheduled run wins: the entry stays due and is retried on the next tick.
func (s *Server) runDueSchedules(now time.Time) {
	if s.buddySchedules == nil || s.buddyJobs == nil {
		return
	}
	for _, entry := range s.buddySchedules.list() {
		if !entry.Enabled || !entry.NextRun.IsZero() && entry.NextRun.After(now) {
			continue
		}
		if entry.NextRun.IsZero() {
			// Never computed (older store): schedule from now, do not fire yet.
			entry.NextRun = computeNextRun(entry, now)
			_ = s.buddySchedules.put(entry)
			continue
		}
		s.startScheduleJob(entry)
	}
}

// startScheduleJob enqueues a send for one schedule entry.
func (s *Server) startScheduleJob(entry *buddyScheduleEntry) {
	if s.agent == nil {
		log.Printf("buddy scheduler: skipping %s: no agent client", entry.ID)
		return
	}
	if _, err := loadBuddyIdentity(); err != nil {
		log.Printf("buddy scheduler: skipping %s: %v", entry.ID, err)
		return
	}
	if conflict := s.buddyJobs.conflicting(entry.Receiver, entry.Source, entry.Dataset); conflict != nil {
		log.Printf("buddy scheduler: skipping %s: job %s already running", entry.ID, conflict.snapshot().ID)
		return
	}
	raw := true
	ctx, cancel := context.WithCancel(context.Background())
	job := &buddyJob{
		buddyJobPublic: buddyJobPublic{
			ID:         randomJobID(),
			Dataset:    entry.Dataset,
			Source:     entry.Source,
			Receiver:   entry.Receiver,
			PruneKeep:  entry.PruneKeep,
			Raw:        raw,
			ScheduleID: entry.ID,
			State:      buddyJobRunning,
			StartedAt:  time.Now().UTC(),
		},
		ctx:    ctx,
		cancel: cancel,
		done:   make(chan struct{}),
	}
	// Advance nextRun at start so a long run cannot fire twice; the result is
	// recorded on completion.
	func() {
		s.buddySchedules.mu.Lock()
		defer s.buddySchedules.mu.Unlock()
		if e, ok := s.buddySchedules.entries[entry.ID]; ok {
			e.NextRun = computeNextRun(e, time.Now().UTC())
			_ = s.buddySchedules.saveLocked()
		}
	}()
	s.buddyJobs.add(job)
	go s.runBuddySendJob(job)
	log.Printf("buddy scheduler: started job %s for schedule %s (%s → %s)",
		job.ID, entry.ID, entry.Dataset, entry.Receiver)
}
